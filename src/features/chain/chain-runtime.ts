/**
 * Chain runtime — walks a `ChainPlan` left to right, threading ASS
 * content through each step's transform. Only the final result is
 * returned; intermediate state stays in memory (the whole point of
 * the chain feature: no orphan intermediate files on disk).
 *
 * The runtime is UI-agnostic — the same executor serves Shape A
 * (chain CLI keyword + `+` separator, current target) and Shape C
 * (recipe YAML, future). Both UIs produce a `ChainPlan`; the
 * runtime doesn't know which produced it.
 *
 * See `docs/architecture/ssahdrify_cli_design.md` § "v1.4.1 stable
 * 后续用户反馈" feature #4 for the locked architectural decisions.
 */

import { Base64 } from "js-base64";

import { processAssContent } from "../hdr-convert/ass-processor";
import { shiftSubtitles } from "../timing-shift/timing-engine";
import { buildFontEntry } from "../font-embed/ass-uuencode";
import { insertFontsSection } from "../font-embed/font-embedder";
import {
  assertSafeOutputFilename,
  assertSafeOutputPath,
  decomposeInputPath,
  substituteTemplate,
} from "../../lib/path-validation";
import type {
  ChainResult,
  ChainRunRequest,
  ChainStep,
  EmbedStepParams,
  HdrStepParams,
  ShiftStepParams,
} from "./chain-types";

/**
 * Per-step transform: takes the in-flight ASS content + original
 * input path, returns the next in-flight content plus optional
 * diagnostic notes.
 *
 * Synchronous — matches the existing engine's call boundary, where
 * the Rust shell's `call_engine` helper invokes a sync JS function
 * via `execute_script`. Embed-in-chain keeps this sync shape:
 * font resolution happens in the Rust shell BEFORE calling
 * runChain (planFontEmbed result returned to Rust → fonts resolved
 * → applyFontEmbed-style payload bundled into EmbedStepParams),
 * not as a TS-side async callback to Rust ops.
 */
type StepTransform<P> = (ctx: TransformContext, params: P) => TransformResult;

interface TransformContext {
  /** ASS content currently in flight. */
  content: string;
  /** Original input path. Stays the same across all steps — it's
   *  the conceptual "what file are we processing," not the in-flight
   *  state's path. */
  inputPath: string;
}

interface TransformResult {
  content: string;
  /** Optional human-readable note for the summary report. */
  note?: string;
}

/**
 * Registry — one entry per `StepKind`. The mapped type binds each
 * kind's transform to its own params variant, so adding a new step
 * is type-checked at registration: a missing entry or a mismatched
 * params type fails to compile.
 *
 * Adding a new step type requires:
 *   1. Extend `StepKind` and `ChainStep` in chain-types.ts
 *   2. Add an entry here with the matching transform function
 *   3. (Rust side) Add a `ParsedStep` variant in chain.rs
 * Three-line surface; that's the full extensibility cost.
 */
const TRANSFORMS: {
  [K in ChainStep["kind"]]: StepTransform<Extract<ChainStep, { kind: K }>["params"]>;
} = {
  hdr: hdrTransform,
  shift: shiftTransform,
  embed: embedTransform,
};

function hdrTransform(ctx: TransformContext, params: HdrStepParams): TransformResult {
  // Direct call to the underlying ASS color processor — bypasses
  // convertHdr's outputPath computation, which is meaningless in a
  // chain (only the chain-global terminal output path matters).
  // For v1, chain assumes ASS content; SRT inputs that need HDR
  // conversion should run `hdr` standalone first.
  const content = processAssContent(ctx.content, params.brightness, params.eotf);
  return { content };
}

function shiftTransform(ctx: TransformContext, params: ShiftStepParams): TransformResult {
  const result = shiftSubtitles(ctx.content, {
    offsetMs: params.offsetMs,
    thresholdMs: params.thresholdMs,
  });
  // `ShiftResult` doesn't carry a `shiftedCount` directly — derive it
  // from the preview array's `wasShifted` flags, matching how the
  // existing `convertShift` wrapper in cli-engine-entry.ts does it.
  const shiftedCount = result.preview.filter((entry) => entry.wasShifted).length;
  const note =
    `shift: ${shiftedCount}/${result.captionCount} entries shifted ` + `(format: ${result.format})`;
  return { content: result.content, note };
}

function embedTransform(ctx: TransformContext, params: EmbedStepParams): TransformResult {
  // Pre-resolution contract: the Rust shell calls planFontEmbed +
  // font lookup + subset_font BEFORE runChain (against the original
  // input content — HDR/Shift don't change [V4+ Styles] Fontname or
  // dialogue \fn references, so pre-resolution is safe), then injects
  // the subsetted bytes into params.subsets. This lets the transform
  // stay sync (matching every other engine call boundary) without
  // needing async TS→Rust callbacks mid-chain.
  if (params.subsets === undefined) {
    throw new Error(
      "embed step in chain requires pre-resolved font subsets " +
        "(params.subsets is undefined — likely a CLI/runtime version " +
        "mismatch where Rust shell didn't pre-resolve)"
    );
  }

  // Empty subsets array is legitimate — subtitle has no font
  // references, or all lookups failed under `--on-missing warn`.
  // Skip the [Fonts] section insertion in that case (matches
  // applyFontEmbed's fast-path in cli-engine-entry.ts).
  if (params.subsets.length === 0) {
    return {
      content: ctx.content,
      note: "embed: 0 fonts embedded (no resolvable references)",
    };
  }

  const fontEntries = params.subsets.map((s) =>
    buildFontEntry(s.fontName, decodeBase64(s.dataB64, s.fontName))
  );
  const fontsSection = `[Fonts]\n${fontEntries.join("\n\n")}\n`;
  const content = insertFontsSection(ctx.content, fontsSection);
  const note = `embed: ${fontEntries.length} font(s) embedded`;
  return { content, note };
}

/**
 * Decode the Rust shell's base64-encoded subset bytes into a
 * Uint8Array. Pairs with the `dataB64` field on `ChainFontSubsetPayload`;
 * the previous JSON-array form (`Uint8Array.from(number[])`) expanded
 * the V8 source ~4-5× per byte and pressured the heap on the worst-
 * case CUMULATIVE_FALLBACK_BYTES path.
 *
 * Uses `js-base64` instead of the global `atob()`: the CLI runs on a
 * bare `deno_core::JsRuntime` with `extensions: vec![]`, which does
 * NOT provide Web APIs. `atob` would throw `ReferenceError: atob is
 * not defined` in production even though Vitest passes because Node
 * has a global atob.
 *
 * `name` annotates errors: a corrupt subset payload from a future
 * Rust-side encoder bug surfaces as `"base64 decode failed for font
 * subset 'XYZ': ..."` rather than a bare error with no font / step
 * attribution.
 */
function decodeBase64(b64: string, name: string): Uint8Array {
  try {
    return Base64.toUint8Array(b64);
  } catch (e) {
    const message = e instanceof Error ? e.message : String(e);
    // `name` is sanitized: it comes from `s.fontName` which is
    // produced by buildFontFileName (font-embedder.ts) — that helper
    // already strips everything but `[a-z0-9_-]` and falls back to a
    // pure-ASCII hash for empty results. BiDi / zero-width chars
    // can't survive that pipeline, so this interpolation is safe even
    // for P1b hostile font packs (N-R5-FECHAIN-17). If a future
    // refactor lets the original family name flow through here
    // unsanitized, gate it through `stripUnicodeControls`.
    throw new Error(`base64 decode failed for font subset '${name}': ${message}`, {
      cause: e,
    });
  }
}

/**
 * Execute a chain plan against a single input file's content.
 *
 * Single-payload signature matches the deno_core call boundary —
 * the Rust shell's `call_engine` helper packs the request as one
 * JSON-serializable object on `globalThis.__ssahdrifyCliPayload`.
 *
 * Errors propagate per locked failure model: any step throwing
 * aborts this file's chain immediately. The Rust shell catches the
 * thrown error, attributes it to the input file, and continues
 * with the next file (skip + continue + report semantics, matching
 * the existing per-feature CLI).
 */
export function runChain(request: ChainRunRequest): ChainResult {
  const { plan, inputPath, content } = request;
  const notes: string[] = [];
  let current = content;

  for (let i = 0; i < plan.steps.length; i++) {
    const step = plan.steps[i];
    const transform = TRANSFORMS[step.kind];
    let result: TransformResult;
    try {
      // The cast is necessary because TypeScript can't statically
      // prove that `transform`'s params type matches `step.params`
      // through the indexed access — but the registry's mapped type
      // above guarantees the correspondence by construction. The
      // runtime correctness is unchanged; this is a TS limitation.
      result = (transform as StepTransform<unknown>)({ content: current, inputPath }, step.params);
    } catch (err) {
      // Annotate which step failed so the Rust shell's error
      // reporting can show "step 2 (shift) failed: ..." rather than
      // a bare engine error. `cause` preserves the original error
      // for downstream debugging without losing the annotated
      // user-facing message.
      const message = err instanceof Error ? err.message : String(err);
      throw new Error(`step ${i + 1} (${step.kind}) failed: ${message}`, {
        cause: err,
      });
    }
    current = result.content;
    if (result.note) {
      notes.push(result.note);
    }
  }

  const outputPath = resolveChainOutputPath(inputPath, plan.outputTemplate);
  return { content: current, outputPath, notes };
}

/**
 * Resolve the chain's terminal output path. Mirrors the per-feature
 * resolvers in shape — uses the shared `decomposeInputPath` helper
 * for absolute-path validation and drive-root handling, plus the
 * shared safety asserts (reserved names, traversal, MAX_PATH,
 * self-overwrite).
 *
 * Supported template tokens: `{name}` (input stem), `{ext}` (input
 * extension with leading dot). Per-step tokens (`{eotf}`,
 * `{format}`) are deliberately NOT supported at chain level — which
 * step's value should they take? The Rust shell's stacked default
 * uses neither, so the most-common path doesn't need them.
 */
export function resolveChainOutputPath(inputPath: string, template: string): string {
  const { dir, baseName, ext, normalized, usedBackslash } = decomposeInputPath(inputPath);

  const outputName = substituteTemplate(template, { name: baseName, ext });

  assertSafeOutputFilename(outputName);
  const outputPath = `${dir}/${outputName}`;
  assertSafeOutputPath(outputPath, normalized);

  return usedBackslash ? outputPath.replace(/\//g, "\\") : outputPath;
}

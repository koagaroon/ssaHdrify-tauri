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
 * See `docs/architecture/ssahdrify_cli_design.md` § "Project design
 * locks → chain feature" for the locked architectural decisions
 * (Shape A / Shape C separation, embed-cap, step-order check, etc.).
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
import { sanitizeError } from "../../lib/dedup-helpers";
import { stripUnicodeControls } from "../../lib/unicode-controls";
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
  //
  // Round 8 A-R8-A1-7: explicit ASS-shape guard. Without it, a raw SRT
  // fed into `chain hdr ...` runs processAssContent on text that has
  // no `[V4+ Styles]` / `[Events]` sections, producing garbage output
  // that's neither ASS nor SRT. Violates no-silent-action: chain
  // either succeeds with the documented contract or surfaces the
  // mismatch. The probe is shape-only — any line starting with
  // `[Script Info]` or `[V4+ Styles]` qualifies; both real ASS files
  // open with at least one of those headers (allowing leading BOM /
  // whitespace / comments).
  // Round 11 W11.7 (A1-R11-05): bound the whitespace runs explicitly.
  // Real ASS headers carry no leading whitespace, and renderers
  // tolerate at most a tab or two before / inside the bracket. {0,16}
  // is generous past anything legitimate and keeps the regex out of
  // catastrophic-backtracking territory for crafted inputs (per the
  // chain `.replace(timingRe)` Codex ReDoS class). Same shape applies
  // to the embed preflight regex above; both share this bound.
  if (!/^\s{0,16}\[\s{0,16}(Script Info|V4\+? Styles)\s{0,16}\]/im.test(ctx.content)) {
    throw new Error(
      "hdr step requires ASS / SSA content (no [Script Info] or " +
        "[V4+ Styles] header found). Run `hdr` standalone first to " +
        "convert SRT / VTT / SUB to ASS, then chain on the result."
    );
  }
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
  // R12 N-R12-8: surface skippedCount in the chain note when > 0.
  // Mirrors TimingShift.tsx's msg_oversized_skipped warning — the
  // chain note is the only user-visible signal the CLI emits for a
  // chain step's diagnostics, so silently dropping skipped count
  // here means oversized captions vanish without trace.
  const baseNote =
    `shift: ${shiftedCount}/${result.captionCount} entries shifted (format: ${result.format})`;
  const note =
    result.skippedCount > 0
      ? `${baseNote}; ${result.skippedCount} oversized caption(s) skipped (>64 KB cap)`
      : baseNote;
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
    // `message` is BiDi-scrubbed via sanitizeError (Round 8 N2 catch-arm
    // sweep): even though Base64.toUint8Array's error message is library-
    // controlled today, the message ends up in a re-thrown Error that
    // surfaces in the chain log panel. Scrubbing at the extraction site
    // keeps the catch-arm contract uniform with the rest of the project.
    //
    // Round 11 W11.7 (A1-R11-06): wrap `name` in stripUnicodeControls
    // too. The N-R5-FECHAIN-17 analysis (buildFontFileName already
    // strips everything but [a-z0-9_-]) is correct for the current
    // call path, but the same "future refactor lets the original name
    // flow through unsanitized" caveat was already documented in the
    // prior comment — the cheap wrap closes the door instead of
    // leaving a "if you ever do X, do Y" reminder.
    const message = sanitizeError(e);
    const safeName = stripUnicodeControls(name);
    throw new Error(`base64 decode failed for font subset '${safeName}': ${message}`, {
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

  // Round 11 W11.3 (N1-R11-04): chain-level preflight for the strictest
  // step's input requirement. hdrTransform accepts content with either
  // [Script Info] OR [V4+ Styles] as the ASS-shape probe, but
  // insertFontsSection (the embed step's terminal call) requires
  // [Script Info] specifically. Without this preflight, a chain like
  // `hdr + embed` on a [V4+ Styles]-only input runs hdrTransform's
  // color transform first, only for embed to throw afterwards — wasted
  // work and the error attribution names step 2 (embed) instead of
  // surfacing "chain shape needs Script Info" upfront. Probe regex is
  // identical to insertFontsSection's gate; keeping the two consistent
  // means a future loosening must touch both sites.
  if (plan.steps.some((s) => s.kind === "embed")) {
    if (!/^\[Script Info\][ \t]*$/im.test(content)) {
      throw new Error(
        "Chain includes an embed step but input ASS has no [Script Info] " +
          "section header. Re-parse / rebuild the file before chaining."
      );
    }
  }

  for (let i = 0; i < plan.steps.length; i++) {
    const step = plan.steps[i];
    const transform = TRANSFORMS[step.kind];
    // Round 11 W11.7 (A1-R11-07): runtime cross-check that the
    // registry actually has a transform for this kind. The TS type
    // system ensures `step.kind` is a `StepKind` union member, so the
    // index access should never miss in well-typed code; but a malformed
    // chain plan deserialized from the Rust shell (or a future
    // StepKind addition that forgets to wire up TRANSFORMS) would slip
    // through as `transform = undefined`, then trip a generic
    // "transform is not a function" downstream. Surfacing the
    // attribution here gives debuggers a chain-shape message instead
    // of a JS-internal one.
    if (!transform) {
      throw new Error(`step ${i + 1} (${step.kind}) has no transform registered`);
    }
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
      // user-facing message. Round 9 N-R9-N1-1 — `message` goes
      // through sanitizeError (Pattern 1 callsite census miss from
      // Wave 8.1): the re-thrown error flows to the chain log panel
      // where any BiDi / line-break smuggling from a P1b transform-
      // internal error would otherwise reach the UI un-scrubbed.
      const message = sanitizeError(err);
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
  // Round 8 N-R8-N1-2: explicit unknown-token reject. substituteTemplate
  // silently substitutes unknown tokens to ""; without this check, a
  // template like `{name}.{eotf}.ass` (using a per-step token at the
  // chain level) would collapse to `{name}.ass` after the empty
  // substitution + boundary-dot trim, with no signal to the user that
  // `{eotf}` was dropped. No-silent-action: surface the bad token at
  // resolution time. The doc comment above already calls out the
  // {eotf} / {format} restriction; this matches it with a runtime
  // gate.
  const CHAIN_ALLOWED_TOKENS = new Set(["name", "ext"]);
  // Round 11 W11.7 (N1-R11-06): widen the token-name regex to include
  // uppercase + underscore-only starts so `{Format}` / `{NAME}` /
  // `{Eotf}` hit the unknown-token error path here rather than slipping
  // past silently. Pre-R11 the lowercase-only `[a-z_][a-z0-9_]*`
  // refused to even match capitalized tokens; substituteTemplate
  // downstream then either left them as literal text or matched only
  // the lowercase variant, with no signal that the capitalized form
  // was an unrecognized intent. The whitelist stays lowercase so a
  // mixed-case input correctly hits this error path.
  // Round 11 W11.7 (A1-R11-01): bound the identifier run to {0,31}
  // (32 chars total including the leading char). Real tokens are short
  // ("name", "ext", "eotf"); a multi-MB unbounded run inside `{...}`
  // would burn iteration cost in matchAll's lexer without ever matching
  // a real token. Defense-in-depth alongside the per-line size cap.
  for (const match of template.matchAll(/\{([a-zA-Z_][a-zA-Z0-9_]{0,31})\}/g)) {
    if (!CHAIN_ALLOWED_TOKENS.has(match[1])) {
      throw new Error(
        `chain output template references unknown token '{${match[1]}}'; ` +
          `chain-level templates support {name} and {ext} only (per-step tokens ` +
          `like {eotf} / {format} are not chain-resolvable)`
      );
    }
  }

  const { dir, baseName, ext, normalized, usedBackslash } = decomposeInputPath(inputPath);

  const outputName = substituteTemplate(template, { name: baseName, ext });

  assertSafeOutputFilename(outputName);
  const outputPath = `${dir}/${outputName}`;
  assertSafeOutputPath(outputPath, normalized);

  return usedBackslash ? outputPath.replace(/\//g, "\\") : outputPath;
}

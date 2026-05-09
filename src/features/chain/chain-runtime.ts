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

import { processAssContent } from "../hdr-convert/ass-processor";
import { shiftSubtitles } from "../timing-shift/timing-engine";
import {
  assertSafeOutputFilename,
  assertSafeOutputPath,
  decomposeInputPath,
} from "../../lib/path-validation";
import type {
  ChainPlan,
  ChainResult,
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
 * Async because future steps (notably embed once wired) need to
 * call back to Rust ops for font subset resolution. Returning a
 * Promise from the start keeps the registry shape stable when
 * embed's real implementation lands.
 */
type StepTransform<P> = (
  ctx: TransformContext,
  params: P
) => Promise<TransformResult>;

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
  [K in ChainStep["kind"]]: StepTransform<
    Extract<ChainStep, { kind: K }>["params"]
  >;
} = {
  hdr: hdrTransform,
  shift: shiftTransform,
  embed: embedTransform,
};

async function hdrTransform(
  ctx: TransformContext,
  params: HdrStepParams
): Promise<TransformResult> {
  // Direct call to the underlying ASS color processor — bypasses
  // convertHdr's outputPath computation, which is meaningless in a
  // chain (only the chain-global terminal output path matters).
  // For v1, chain assumes ASS content; SRT inputs that need HDR
  // conversion should run `hdr` standalone first.
  const content = processAssContent(ctx.content, params.brightness, params.eotf);
  return { content };
}

async function shiftTransform(
  ctx: TransformContext,
  params: ShiftStepParams
): Promise<TransformResult> {
  const result = shiftSubtitles(ctx.content, {
    offsetMs: params.offsetMs,
    thresholdMs: params.thresholdMs,
  });
  // `ShiftResult` doesn't carry a `shiftedCount` directly — derive it
  // from the preview array's `wasShifted` flags, matching how the
  // existing `convertShift` wrapper in cli-engine-entry.ts does it.
  const shiftedCount = result.preview.filter((entry) => entry.wasShifted).length;
  const note =
    `shift: ${shiftedCount}/${result.captionCount} entries shifted ` +
    `(format: ${result.format})`;
  return { content: result.content, note };
}

async function embedTransform(
  ctx: TransformContext,
  params: EmbedStepParams
): Promise<TransformResult> {
  // Embed in chain requires font-resolution callback to Rust ops
  // (find_system_font + subset_font), wired in a follow-up. Standalone
  // `embed` subcommand still works as today; users wanting embed in
  // a chain wait until step 5 of the implementation order lands.
  void ctx;
  void params;
  throw new Error(
    "embed step in chain is not yet implemented (font resolution callback pending)"
  );
}

/**
 * Execute a chain plan against a single input file's content.
 *
 * Errors propagate per locked failure model: any step throwing
 * aborts this file's chain immediately. The Rust shell catches the
 * thrown error, attributes it to the input file, and continues with
 * the next file (skip + continue + report semantics, matching the
 * existing per-feature CLI).
 */
export async function runChain(
  plan: ChainPlan,
  inputPath: string,
  content: string
): Promise<ChainResult> {
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
      result = await (transform as StepTransform<unknown>)(
        { content: current, inputPath },
        step.params
      );
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

  const outputName = template
    .replace(/\{name\}/g, baseName)
    .replace(/\{ext\}/g, ext)
    .replace(/\.{2,}/g, ".");

  assertSafeOutputFilename(outputName);
  const outputPath = `${dir}/${outputName}`;
  assertSafeOutputPath(outputPath, normalized);

  return usedBackslash ? outputPath.replace(/\//g, "\\") : outputPath;
}

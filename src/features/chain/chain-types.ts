/**
 * Chain feature — types shared across the runtime, the Rust-side
 * argv parser (mirrored in serde), and downstream UIs (Shape A
 * chain CLI today; Shape C recipe-file later).
 *
 * The AST is flat by design: an ordered list of steps, no nesting.
 * Future complexity (conditionals, sub-pipelines) can extend this
 * schema without breaking the v1 shape.
 *
 * See `docs/architecture/ssahdrify_cli_design.md` § "Project design
 * locks → chain feature" for the locked design decisions.
 */

import type { Eotf } from "../hdr-convert/color-engine";

/**
 * One kind per chainable subcommand. Adding a new feature: extend
 * this union, the discriminated `ChainStep` below, and the
 * `TRANSFORMS` registry in chain-runtime.ts.
 *
 * `rename` is intentionally NOT chainable — its batch-coordinator
 * shape (N subtitles + M videos → N pairings) does not compose
 * with the stream-transform model. Users wanting rename + chain
 * run them as two invocations.
 */
export type StepKind = "hdr" | "shift" | "embed";

export interface HdrStepParams {
  eotf: Eotf;
  brightness: number;
}

export interface ShiftStepParams {
  offsetMs: number;
  thresholdMs?: number;
}

/**
 * Embed step parameters carried in the chain runtime payload.
 *
 * Round 10 N-R10-017: `fontDirs`, `fontFiles`, `noSystemFonts`, and
 * `onMissing` are populated by the Rust shell but NEVER read by the
 * TS embed transform — Rust resolves all four upstream and bakes
 * the result into `subsets` before invoking runChain, so the TS side
 * sees only the pre-resolved bytes. The fields stay on the payload
 * shape for two reasons:
 *
 * 1. Wire-format symmetry with `cli/chain.rs::ParsedStep::Embed` —
 *    the Rust struct serializes all four; dropping them on the TS
 *    side would force a divergent payload spec and complicate the
 *    JSON-round-trip test.
 * 2. Future GUI integration. When the chain runtime gets a GUI
 *    surface (Shape A → Shape B/C per the CLI design doc), the GUI
 *    may resolve fonts in TS rather than Rust — at that point the
 *    TS embed transform will start reading these fields directly.
 *
 * Splitting into Rust-only / TS-only param shapes would force the
 * payload type to switch by caller, adding wire-format branching for
 * a future-but-unused symmetry. Keep the unified shape with this
 * note instead.
 */
export interface EmbedStepParams {
  fontDirs: string[];
  fontFiles: string[];
  noSystemFonts: boolean;
  onMissing: "warn" | "fail";
  /**
   * Pre-resolved font subsets, populated by the Rust shell BEFORE
   * runChain is invoked. The TS embed transform uses these directly
   * (skipping planFontEmbed and font lookup, which the Rust shell
   * already did against the original input content — HDR/Shift don't
   * change which fonts are referenced, so pre-resolution is safe).
   *
   * Empty array means "no fonts to embed" (subtitle has no font
   * references, or all lookups failed under `--on-missing warn`).
   * Undefined means "Rust didn't pre-resolve" — the embed transform
   * errors with a helpful message in that case (likely a
   * runtime/CLI version mismatch).
   */
  subsets?: ChainFontSubsetPayload[];
}

/**
 * Chain-mode font subset payload: base64-encoded bytes. Renamed from
 * the original `FontSubsetPayload` to disambiguate from the
 * standalone-embed payload of the same name in
 * `src/cli-engine-entry.ts`, which uses the JSON `number[]` form.
 * The two coexist intentionally (different IPC paths, different
 * expansion-vs-compatibility tradeoffs); the rename stops IDE
 * auto-import from picking the wrong one.
 */
export interface ChainFontSubsetPayload {
  fontName: string;
  /**
   * Subset bytes encoded as base64. The Rust shell encodes `Vec<u8>`
   * into a base64 string (~1.33× expansion) instead of the previous
   * JSON `[byte, byte, ...]` form (~4-5× expansion that pressured V8's
   * heap on the worst-case CUMULATIVE_FALLBACK_BYTES path). The embed
   * transform decodes via `js-base64`; the bare deno_core JsRuntime
   * has no `atob` global, and the GUI's `subsetFont` decodes the same
   * way so both paths share one decoder.
   */
  dataB64: string;
}

/**
 * Discriminated union — `step.kind` narrows `step.params` to the
 * matching variant. Keeps the registry in chain-runtime.ts type-safe
 * without runtime casts at callsite.
 */
export type ChainStep =
  | { kind: "hdr"; params: HdrStepParams }
  | { kind: "shift"; params: ShiftStepParams }
  | { kind: "embed"; params: EmbedStepParams };

/**
 * The complete chain plan. Runtime walks `steps` left to right.
 * `outputTemplate` is chain-global — applied only at the terminal
 * step's output. Non-terminal steps' outputs stay in memory.
 */
export interface ChainPlan {
  steps: ChainStep[];
  /**
   * Chain-global output template. Defaults to a stacked-suffix form
   * (`{name}.hdr.shifted.embed.ass`) when the user didn't pass
   * `--output-template`; the Rust shell pre-computes the default and
   * passes the resolved string here, so the runtime does not need
   * to know about CLI defaults.
   *
   * Supported tokens for chain output: `{name}`, `{ext}`. Other
   * per-step tokens (`{eotf}`, `{format}`) are not meaningful at
   * chain level (which step's value would they take?).
   */
  outputTemplate: string;
}

/**
 * Single-payload request shape for `runChain` at the deno_core
 * boundary. Pack `plan`, `inputPath`, and `content` into one object
 * so the Rust shell's `call_engine` helper (which marshals one
 * payload via `globalThis.__ssahdrifyCliPayload`) can reach the
 * runtime without needing a multi-arg JS calling convention.
 */
export interface ChainRunRequest {
  plan: ChainPlan;
  inputPath: string;
  content: string;
}

/**
 * Result returned to the Rust shell after `runChain` completes.
 */
export interface ChainResult {
  /** Final ASS content after all steps; ready for Rust to write. */
  content: string;
  /** Output path resolved against the chain's `outputTemplate`. */
  outputPath: string;
  /**
   * Per-step diagnostic notes (e.g., "shift: 3 of 12 entries shifted",
   * "embed: 2 fonts missing"). Surfaced in the CLI summary report.
   * Empty array if no notes.
   */
  notes: string[];
  /**
   * Aggregate count of captions whose text exceeded
   * MAX_CAPTION_TEXT_LEN (64 KB) across every step that parses
   * subtitle content (today: only `shift`). Round 13 N-R13-1 — the
   * Rust shell reads this directly and routes it through
   * `emit_oversized_skipped_warning` (stderr + FileReport.warnings),
   * mirroring the standalone HDR / Shift CLI paths. Previously the
   * count was embedded as a suffix in `notes[]`, which the Rust
   * shell never parsed and which printed to stdout (notes loop),
   * not stderr where warnings belong.
   */
  skippedCount: number;
}

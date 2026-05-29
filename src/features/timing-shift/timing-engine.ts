/**
 * Timing shift engine — browser-compatible subtitle timing adjustment.
 *
 * Uses our custom subtitle-parser (no Node.js dependencies).
 *
 * Supports two modes:
 * 1. Simple: shift all timestamps by a fixed offset
 * 2. Threshold: shift only timestamps after a given time point
 *
 * Formats: ASS, SRT, VTT, SUB (MicroDVD)
 */
import {
  shiftSubtitle,
  formatDisplayTime,
  parseDisplayTime,
  type Caption,
  type SubtitleFormat,
} from "../../lib/subtitle-parser";
import {
  assertSafeOutputFilename,
  assertSafeOutputPath,
  decomposeInputPath,
} from "../../lib/path-validation";

export type { Caption, SubtitleFormat };
export { formatDisplayTime, parseDisplayTime };

export interface ShiftOptions {
  /** Offset in milliseconds (positive = later/slower, negative = earlier/faster) */
  offsetMs: number;
  /** If set, only shift captions starting at or after this timestamp (ms) */
  thresholdMs?: number | undefined;
  /** Frame rate for frame-based formats (SUB/MicroDVD). Defaults to 23.976. */
  fps?: number | undefined;
}

/**
 * Validate the numeric shift params at a CLI engine boundary. Deliberately
 * NOT called inside `shiftSubtitles`: the GUI passes NumberInput-validated
 * values, and the formatter already clamps a stray NaN to 0 — but at the CLI
 * entry that clamp is a MISLEADING success (shiftedCount / preview disagree
 * with the rendered zero-shift). Callers at the trust boundary (the standalone
 * `convertShift` and the chain shift transform) reject up front instead, so
 * the failure is loud rather than a silent no-op.
 */
export function assertFiniteShiftMs(offsetMs: number, thresholdMs?: number): void {
  if (!Number.isFinite(offsetMs)) {
    throw new Error(`Invalid offsetMs: expected a finite number, got ${String(offsetMs)}`);
  }
  if (thresholdMs !== undefined && !Number.isFinite(thresholdMs)) {
    throw new Error(`Invalid thresholdMs: expected a finite number, got ${String(thresholdMs)}`);
  }
}

export interface PreviewEntry {
  index: number;
  originalStart: number;
  originalEnd: number;
  shiftedStart: number;
  shiftedEnd: number;
  /** Truncated text for DOM efficiency (max ~60 codepoints). Keep this for display. */
  text: string;
  /** Full un-truncated text — used for hover tooltips so long lines remain readable. */
  fullText: string;
  wasShifted: boolean;
}

export interface ShiftResult {
  /** Shifted subtitle content as string */
  content: string;
  /** Detected format name */
  format: SubtitleFormat;
  /** Preview entries: original and shifted timings */
  preview: PreviewEntry[];
  /** Total number of captions (includes skipped placeholders) */
  captionCount: number;
  /**
   * Count of captions whose text exceeded MAX_CAPTION_TEXT_LEN (64 KB)
   * and were emitted as skipped placeholders by the parser. TimingShift
   * surfaces this via msg_oversized_skipped to close the
   * no-silent-action gap.
   */
  skippedCount: number;
}

/**
 * Parse, shift, and rebuild a subtitle file.
 */
export function shiftSubtitles(content: string, options: ShiftOptions): ShiftResult {
  const { offsetMs, thresholdMs, fps } = options;

  const { output, format, captions, shifted } = shiftSubtitle(content, offsetMs, thresholdMs, fps);

  // Build preview for every caption — the UI scroll container decides
  // how many are visible at a time. Long lines are truncated to 60
  // codepoints (not UTF-16 code units) so emoji and astral-plane glyphs
  // aren't bisected mid-surrogate. The full text is preserved in fullText
  // for hover tooltips.
  const truncateCodepoints = (text: string, max: number): string => {
    let cp = 0;
    let out = "";
    for (const ch of text) {
      if (cp >= max) break;
      out += ch;
      cp++;
    }
    return out;
  };
  // (Pattern 2): exclude `skipped: true` placeholders
  // (oversized captions kept positionally for buildAss's sequential consume
  // — see subtitle-parser.ts MAX_CAPTION_TEXT_LEN handling). The preview
  // surface is a user-facing list; including placeholder rows allocates
  // up to MAX_PARSED_ENTRIES (500k) empty PreviewEntry objects with no
  // visible content. Build paths (HDR / Embed / output writing) already
  // filter `c.skipped`; preview was the lone consumer that didn't.
  // `index` still tracks the caption's position in the buildAss-aligned
  // vector so the displayed numbering matches the source line numbers a
  // user might cross-reference.
  const preview: PreviewEntry[] = [];
  for (let i = 0; i < captions.length; i++) {
    const c = captions[i]!;
    if (c.skipped) continue;
    const s = shifted[i]!;
    const wasShifted = c.start !== s.start || c.end !== s.end;
    preview.push({
      index: i + 1,
      originalStart: c.start,
      originalEnd: c.end,
      shiftedStart: s.start,
      shiftedEnd: s.end,
      text: truncateCodepoints(c.text, 60),
      fullText: c.text,
      wasShifted,
    });
  }

  return {
    content: output,
    format,
    preview,
    captionCount: captions.length,
    skippedCount: captions.filter((c) => c.skipped).length,
  };
}

/**
 * Derive the `.shifted` output path for a given input subtitle path.
 *
 * Used by the batch save flow — Time Shift writes outputs alongside
 * inputs with a `.shifted` infix, preserving the original extension
 * (`EP01.srt` → `EP01.shifted.srt`, `EP01.ass` → `EP01.shifted.ass`).
 * The native separator of the input path is preserved so the result
 * round-trips through Win32 APIs and shell-integration tools without
 * mixing slashes.
 *
 * Why a derived path instead of a per-file native save dialog: those
 * dialogs are blocking and don't scale to N files. The same-directory
 * convention matches the most common workflow (shift in place beside
 * the existing subs) and gives the user a single overwrite-confirm
 * gate via `countExistingFiles` before the batch begins.
 */
export function deriveShiftedPath(inputPath: string): string {
  // Decompose via the shared helper. Validates absolute, accepts drive-
  // root files (`C:\foo.ass`), rejects drive-relative (`C:foo.ass`).
  const parts = decomposeInputPath(inputPath);
  const { dir, ext, normalized, usedBackslash } = parts;
  let { baseName } = parts;
  // Strip any prior `.shifted` infix so re-shifting `EP01.shifted.ass`
  // yields `EP01.shifted.ass` (idempotent) rather than the cumulative
  // `EP01.shifted.shifted.ass`. Mirrors the strip-and-re-apply pattern
  // resolveOutputPath uses for the HDR `.hdr` infix.
  if (baseName.toLowerCase().endsWith(".shifted")) {
    baseName = baseName.slice(0, -".shifted".length);
  }
  // after the `.shifted` strip the baseName can
  // be empty (or whitespace-/dot-only — POSIX dotfile shapes like
  // `.shifted.srt` whose stem is just `.`). Without this re-check
  // the output name resolves to `.shifted${ext}` — identical to the
  // input on subsequent invocations and visually weird as a filename.
  // `cli-engine-entry.ts::resolveShiftOutputPathInternal` has the same
  // guard; mirror it here for the GUI path so error messages stay
  // aligned across cheap (this function) and heavy (CLI engine)
  // paths.
  if (!baseName || !baseName.replace(/^\.+/, "").trim()) {
    throw new Error("Input filename has no valid stem after stripping .shifted infix");
  }
  const outputName = `${baseName}.shifted${ext}`;
  // Apply the shared safety checks (reserved names, traversal,
  // MAX_PATH, self-overwrite). Same helpers as HDR / Embed resolvers.
  assertSafeOutputFilename(outputName);
  const outputPath = `${dir}/${outputName}`;
  assertSafeOutputPath(outputPath, normalized);
  return usedBackslash ? outputPath.replace(/\//g, "\\") : outputPath;
}

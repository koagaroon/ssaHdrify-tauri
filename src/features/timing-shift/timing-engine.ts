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

export type { Caption, SubtitleFormat };
export { formatDisplayTime, parseDisplayTime };

export interface ShiftOptions {
  /** Offset in milliseconds (positive = later/slower, negative = earlier/faster) */
  offsetMs: number;
  /** If set, only shift captions starting at or after this timestamp (ms) */
  thresholdMs?: number;
  /** Frame rate for frame-based formats (SUB/MicroDVD). Defaults to 23.976. */
  fps?: number;
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
  /** Total number of captions */
  captionCount: number;
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
  const preview: PreviewEntry[] = captions.map((c, i) => {
    const s = shifted[i];
    const wasShifted = c.start !== s.start || c.end !== s.end;
    return {
      index: i + 1,
      originalStart: c.start,
      originalEnd: c.end,
      shiftedStart: s.start,
      shiftedEnd: s.end,
      text: truncateCodepoints(c.text, 60),
      fullText: c.text,
      wasShifted,
    };
  });

  return {
    content: output,
    format,
    preview,
    captionCount: captions.length,
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
  const usedBackslash = inputPath.includes("\\") && !inputPath.includes("/");
  const normalized = inputPath.replace(/\\/g, "/");
  const lastSlash = normalized.lastIndexOf("/");
  const dir = lastSlash >= 0 ? normalized.slice(0, lastSlash) : "";
  const fullName = lastSlash >= 0 ? normalized.slice(lastSlash + 1) : normalized;
  const lastDot = fullName.lastIndexOf(".");
  const baseName = lastDot > 0 ? fullName.slice(0, lastDot) : fullName;
  const ext = lastDot > 0 ? fullName.slice(lastDot) : "";
  const outputName = `${baseName}.shifted${ext}`;
  const outputPath = dir ? `${dir}/${outputName}` : outputName;
  return usedBackslash ? outputPath.replace(/\//g, "\\") : outputPath;
}

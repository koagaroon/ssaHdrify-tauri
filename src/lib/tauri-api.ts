/**
 * Thin wrappers around Tauri IPC for file I/O and dialogs.
 * Centralizes all native interactions so feature code stays pure JS.
 */
import { open, save } from "@tauri-apps/plugin-dialog";
import {
  readFile,
  stat,
  writeTextFile,
} from "@tauri-apps/plugin-fs";
import { invoke } from "@tauri-apps/api/core";

// ── File Dialogs ──────────────────────────────────────────

export interface FileFilter {
  name: string;
  extensions: string[];
}

const SUBTITLE_FILTERS: FileFilter[] = [
  { name: "ASS/SSA Subtitles", extensions: ["ass", "ssa"] },
  { name: "SRT Subtitles", extensions: ["srt"] },
  { name: "SUB (MicroDVD)", extensions: ["sub"] },
  { name: "WebVTT", extensions: ["vtt"] },
  { name: "All Subtitle Formats", extensions: ["ass", "ssa", "srt", "sub", "vtt", "sbv", "lrc"] },
  { name: "All Files", extensions: ["*"] },
];

const ASS_FILTERS: FileFilter[] = [
  { name: "ASS/SSA Subtitles", extensions: ["ass", "ssa"] },
  { name: "All Files", extensions: ["*"] },
];

/** Open a multi-file picker for subtitle files. Returns file paths or null if cancelled. */
export async function pickSubtitleFiles(): Promise<string[] | null> {
  const result = await open({
    multiple: true,
    filters: SUBTITLE_FILTERS,
    title: "Select subtitle files",
  });
  if (!result) return null;
  // open() returns string | string[] | null depending on multiple flag
  return Array.isArray(result) ? result : [result];
}

/** Open a single-file picker for ASS files. */
export async function pickAssFile(): Promise<string | null> {
  const result = await open({
    multiple: false,
    filters: ASS_FILTERS,
    title: "Select .ass file",
  });
  if (!result) return null;
  return typeof result === "string" ? result : result[0] ?? null;
}

/** Open a single-file picker for any subtitle format. */
export async function pickSubtitleFile(): Promise<string | null> {
  const result = await open({
    multiple: false,
    filters: SUBTITLE_FILTERS,
    title: "Select subtitle file",
  });
  if (!result) return null;
  return typeof result === "string" ? result : result[0] ?? null;
}

/** Save dialog — returns chosen path or null if cancelled. */
export async function pickSavePath(
  defaultName: string,
  filters?: FileFilter[]
): Promise<string | null> {
  const result = await save({
    defaultPath: defaultName,
    filters: filters ?? SUBTITLE_FILTERS,
    title: "Save subtitle file",
  });
  return result ?? null;
}

// ── File I/O ──────────────────────────────────────────────

const MAX_BINARY_SIZE = 100 * 1024 * 1024; // 100 MB (font files)

/** Check file size before reading. Throws if file exceeds the limit. */
async function assertFileSize(path: string, maxBytes: number): Promise<void> {
  const info = await stat(path);
  if (info.size > maxBytes) {
    const sizeMB = (info.size / (1024 * 1024)).toFixed(1);
    const limitMB = (maxBytes / (1024 * 1024)).toFixed(0);
    throw new Error(
      `File too large: ${sizeMB} MB exceeds the ${limitMB} MB limit (${path})`
    );
  }
}

/** Result from encoding-aware file reading. */
export interface ReadTextResult {
  /** File content decoded to UTF-8 */
  text: string;
  /** Detected encoding (e.g. "UTF-8", "GBK", "Big5", "Shift_JIS", "UTF-16LE") */
  encoding: string;
}

/**
 * Read a text file with automatic encoding detection.
 *
 * Handles UTF-8, UTF-8 BOM, UTF-16 LE/BE, GBK, Big5, Shift_JIS, EUC-KR,
 * and other encodings via the Rust backend (chardetng + encoding_rs).
 * Returns clean UTF-8 text regardless of original encoding.
 */
export async function readText(path: string): Promise<string> {
  const result = await readTextDetectEncoding(path);
  return result.text;
}

/**
 * Read a text file with encoding detection, returning both text and encoding name.
 * Useful when the UI needs to display the detected encoding.
 */
export async function readTextDetectEncoding(
  path: string
): Promise<ReadTextResult> {
  return invoke<ReadTextResult>("read_text_detect_encoding", { path });
}

/** Write a text file with explicit UTF-8. */
export async function writeText(path: string, content: string): Promise<void> {
  await writeTextFile(path, content);
}

/** Read a binary file (for font files). Returns Uint8Array. */
export async function readBinary(
  path: string,
  maxSizeBytes: number = MAX_BINARY_SIZE
): Promise<Uint8Array> {
  await assertFileSize(path, maxSizeBytes);
  const data = await readFile(path);
  // Post-read size check to close TOCTOU window
  if (data.length > maxSizeBytes) {
    const sizeMB = (data.length / (1024 * 1024)).toFixed(1);
    const limitMB = (maxSizeBytes / (1024 * 1024)).toFixed(0);
    throw new Error(
      `File too large after read: ${sizeMB} MB exceeds the ${limitMB} MB limit (${path})`
    );
  }
  return data;
}

// ── Path Utilities ───────────────────────────────────────

/** Extract the filename from a full file path (handles both / and \ separators). */
export function fileNameFromPath(path: string): string {
  return path.replace(/\\/g, "/").split("/").pop() ?? path;
}

// ── Rust Commands ─────────────────────────────────────────

/** Find a system font file path by family name and style. */
export async function findSystemFont(
  family: string,
  bold: boolean,
  italic: boolean
): Promise<string> {
  return invoke<string>("find_system_font", { family, bold, italic });
}

/** Subset a font file to only include the specified codepoints. */
export async function subsetFont(
  fontPath: string,
  codepoints: number[]
): Promise<Uint8Array> {
  const bytes: number[] = await invoke("subset_font", { fontPath, codepoints });
  return new Uint8Array(bytes);
}

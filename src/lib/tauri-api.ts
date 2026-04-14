/**
 * Thin wrappers around Tauri IPC for file I/O and dialogs.
 * Centralizes all native interactions so feature code stays pure JS.
 */
import { open, save } from "@tauri-apps/plugin-dialog";
import {
  readFile,
  readTextFile,
  writeFile,
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

/** Read a text file with explicit UTF-8 encoding. */
export async function readText(path: string): Promise<string> {
  return readTextFile(path);
}

/** Write a text file with explicit UTF-8. */
export async function writeText(path: string, content: string): Promise<void> {
  await writeTextFile(path, content);
}

/** Read a binary file (for font files). Returns Uint8Array. */
export async function readBinary(path: string): Promise<Uint8Array> {
  return readFile(path);
}

/** Write a binary file. */
export async function writeBinary(
  path: string,
  data: Uint8Array
): Promise<void> {
  await writeFile(path, data);
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

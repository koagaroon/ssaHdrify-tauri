/**
 * Thin wrappers around Tauri IPC for file I/O and dialogs.
 * Centralizes all native interactions so feature code stays pure JS.
 */
import { open } from "@tauri-apps/plugin-dialog";
import { writeTextFile, rename, copyFile } from "@tauri-apps/plugin-fs";
import { invoke, Channel } from "@tauri-apps/api/core";

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

const FONT_FILTERS: FileFilter[] = [
  { name: "Font Files", extensions: ["ttf", "otf", "ttc", "otc"] },
  { name: "All Files", extensions: ["*"] },
];

// open() returns string | string[] | null. These helpers normalize each shape.
function toSinglePath(result: string | string[] | null): string | null {
  if (!result) return null;
  return typeof result === "string" ? result : (result[0] ?? null);
}

function toMultiplePaths(result: string | string[] | null): string[] | null {
  if (!result) return null;
  return Array.isArray(result) ? result : [result];
}

/** Open a multi-file picker for subtitle files. Returns file paths or null if cancelled. */
export async function pickSubtitleFiles(): Promise<string[] | null> {
  return toMultiplePaths(
    await open({ multiple: true, filters: SUBTITLE_FILTERS, title: "Select subtitle files" })
  );
}

/** Open a multi-file picker for ASS files. Used by Font Embed batch flow,
 *  which only applies to ASS/SSA inputs (other subtitle formats don't
 *  carry font references). */
export async function pickAssFiles(): Promise<string[] | null> {
  return toMultiplePaths(
    await open({ multiple: true, filters: ASS_FILTERS, title: "Select .ass files" })
  );
}

const VIDEO_AND_SUBTITLE_FILTERS: FileFilter[] = [
  {
    name: "Video & Subtitle Files",
    extensions: [
      "mp4",
      "mkv",
      "avi",
      "mov",
      "ts",
      "m2ts",
      "webm",
      "flv",
      "wmv",
      "mpg",
      "mpeg",
      "m4v",
      "ass",
      "ssa",
      "srt",
      "sub",
      "vtt",
      "sbv",
      "lrc",
    ],
  },
  {
    name: "Video Files",
    extensions: [
      "mp4",
      "mkv",
      "avi",
      "mov",
      "ts",
      "m2ts",
      "webm",
      "flv",
      "wmv",
      "mpg",
      "mpeg",
      "m4v",
    ],
  },
  {
    name: "Subtitle Files",
    extensions: ["ass", "ssa", "srt", "sub", "vtt", "sbv", "lrc"],
  },
  { name: "All Files", extensions: ["*"] },
];

/** Open a multi-file picker accepting both videos and subtitles. Used by
 *  the Batch Rename tab, which auto-categorizes by extension after pick. */
export async function pickRenameInputs(): Promise<string[] | null> {
  return toMultiplePaths(
    await open({
      multiple: true,
      filters: VIDEO_AND_SUBTITLE_FILTERS,
      title: "Select videos and subtitles",
    })
  );
}

/** Open a directory picker for the Batch Rename "copy to chosen
 *  directory" output mode. Returns absolute path or null on cancel. */
export async function pickOutputDirectory(): Promise<string | null> {
  return toSinglePath(
    await open({ directory: true, multiple: false, title: "Choose output directory" })
  );
}

/** Open a directory picker for a local font folder. Returns path or null. */
export async function pickFontDirectory(): Promise<string | null> {
  return toSinglePath(
    await open({ directory: true, multiple: false, title: "Select font folder" })
  );
}

/** Open a multi-file picker for individual font files. Returns paths or null. */
export async function pickFontFiles(): Promise<string[] | null> {
  return toMultiplePaths(
    await open({ multiple: true, filters: FONT_FILTERS, title: "Select font files" })
  );
}

// ── File I/O ──────────────────────────────────────────────

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
export async function readTextDetectEncoding(path: string): Promise<ReadTextResult> {
  return invoke<ReadTextResult>("read_text_detect_encoding", { path });
}

/** Write a text file with explicit UTF-8. */
export async function writeText(path: string, content: string): Promise<void> {
  await writeTextFile(path, content);
}

/** Rename / move a file. Atomic on the same volume; falls back to the
 *  OS's copy-then-delete on cross-volume targets (Tauri plugin-fs
 *  semantics). Used by Batch Rename's "rename in place" mode where
 *  the source file disappears. Throws on failure — collisions surface
 *  as the OS rejecting the rename, which the caller logs per-file. */
export async function renamePath(from: string, to: string): Promise<void> {
  await rename(from, to);
}

/** Copy a file. Source is preserved. Used by Batch Rename's two copy
 *  modes (copy-to-video-directory / copy-to-chosen). Overwrites the
 *  target if it exists — pre-flight overwrite confirmation lives at
 *  the caller. */
export async function copyPath(from: string, to: string): Promise<void> {
  await copyFile(from, to);
}

// ── Path Utilities ───────────────────────────────────────

/** Extract the filename from a full file path (handles both / and \ separators). */
export function fileNameFromPath(path: string): string {
  return path.replace(/\\/g, "/").split("/").pop() ?? path;
}

// ── Rust Commands ─────────────────────────────────────────

/** Result of font lookup — path + face index for TTC files. */
export interface FontLookupResult {
  /** Absolute path to the font file */
  path: string;
  /** Face index within the file (0 for single fonts, >0 for TTC faces) */
  index: number;
}

/** Find a system font file by family name and style. Returns path + face index. */
export async function findSystemFont(
  family: string,
  bold: boolean,
  italic: boolean
): Promise<FontLookupResult> {
  return invoke<FontLookupResult>("find_system_font", { family, bold, italic });
}

/** Subset a font file to only include the specified codepoints. */
export async function subsetFont(
  fontPath: string,
  fontIndex: number,
  codepoints: number[]
): Promise<Uint8Array> {
  const bytes: number[] = await invoke("subset_font", { fontPath, fontIndex, codepoints });
  return new Uint8Array(bytes);
}

/** One font face discovered in a user-picked directory or file list.
 *
 *  A single face can expose multiple localized family-name variants (common
 *  for CJK fonts that carry both an English and a Chinese name in their
 *  OpenType name table). `families[0]` is the preferred display name; the
 *  rest are kept for matching so an ASS script referring to any variant will
 *  still resolve to the same file.
 */
export interface LocalFontEntry {
  /** Canonical path to the font file */
  path: string;
  /** Face index within the file (0 for TTF/OTF, 0..n for TTC/OTC) */
  index: number;
  /** All localized family names for this face (display name first). */
  families: string[];
  /** True when OS/2 weight >= 600 */
  bold: boolean;
  /** True for Italic/Oblique styles */
  italic: boolean;
  /** File size in bytes (same value repeated across faces of one TTC) */
  sizeBytes: number;
}

/** Streaming progress payload from the Rust scan commands. Frontend
 *  appends `Batch` events to its accumulator until the invoke promise
 *  resolves (success), rejects (error), or the user cancels (resolves
 *  with whatever was accumulated so far). */
type RawScanProgress = { kind: "batch"; entries: RawLocalFontEntry[] };

/** Optional callback for streaming font scan results. Called once per
 *  Rust-side batch (every ~50 faces or ~100ms). The accumulated list
 *  remains available in the resolved return value too — `onBatch` only
 *  exists so the UI can update progressively. */
export type ScanProgressCallback = (delta: LocalFontEntry[], total: number) => void;

/**
 * Scan a user-picked directory (one level deep) for font files. Streams
 * results back via `onBatch`; the resolved value is the same complete list
 * for callers that just want the final result. TTC files produce multiple
 * entries sharing the same path. Each returned path is registered on the
 * Rust side so subset_font will accept it.
 *
 * Cancellation: call {@link cancelFontScan} from a button handler. The
 * Rust scan returns early; the resolved value contains the partial set
 * accumulated up to that point (no rejection — partial preservation is
 * the contract).
 */
export async function scanFontDirectory(
  dir: string,
  onBatch?: ScanProgressCallback
): Promise<LocalFontEntry[]> {
  return runStreamingScan("scan_font_directory", { dir }, onBatch);
}

/** Scan a user-supplied list of individual font file paths. Same streaming
 *  contract as {@link scanFontDirectory}. */
export async function scanFontFiles(
  paths: string[],
  onBatch?: ScanProgressCallback
): Promise<LocalFontEntry[]> {
  return runStreamingScan("scan_font_files", { paths }, onBatch);
}

/** Request the current font scan be cancelled. Idempotent — safe to call
 *  even when no scan is active. The running scan returns its partial list
 *  via the same Promise the caller is awaiting. */
export async function cancelFontScan(): Promise<void> {
  await invoke("cancel_font_scan");
}

/** Shared streaming-invoke wrapper for both scan commands. Constructs a
 *  Channel<ScanProgress>, accumulates batches in-order, and resolves with
 *  the full list once the Rust side returns. */
async function runStreamingScan(
  command: "scan_font_directory" | "scan_font_files",
  args: Record<string, unknown>,
  onBatch?: ScanProgressCallback
): Promise<LocalFontEntry[]> {
  const accumulated: LocalFontEntry[] = [];
  const channel = new Channel<RawScanProgress>();
  channel.onmessage = (msg) => {
    if (msg.kind !== "batch") return;
    const converted = msg.entries.map(fromRawLocalFontEntry);
    accumulated.push(...converted);
    onBatch?.(converted, accumulated.length);
  };
  await invoke(command, { ...args, progress: channel });
  return accumulated;
}

/**
 * Expand a list of paths from a drag-drop event into a flat list of file
 * paths. Folders are walked one level deep; files pass through unchanged.
 * Hidden entries, symlinks, and reparse points are skipped on the Rust
 * side. Returns an empty array when nothing usable was dropped.
 */
export async function expandDroppedPaths(paths: string[]): Promise<string[]> {
  return invoke<string[]>("expand_dropped_paths", { paths });
}

// Rust serializes `size_bytes` (snake_case); translate to camelCase here so
// the rest of the frontend stays in JS conventions.
interface RawLocalFontEntry {
  path: string;
  index: number;
  families: string[];
  bold: boolean;
  italic: boolean;
  size_bytes: number;
}

function fromRawLocalFontEntry(raw: RawLocalFontEntry): LocalFontEntry {
  return {
    path: raw.path,
    index: raw.index,
    families: raw.families,
    bold: raw.bold,
    italic: raw.italic,
    sizeBytes: raw.size_bytes,
  };
}

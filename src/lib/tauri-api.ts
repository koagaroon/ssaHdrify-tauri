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

export type DialogTranslator = (key: string, ...args: (string | number)[]) => string;

const dialogFallbacks: Record<string, string> = {
  dialog_filter_ass_ssa_subtitles: "ASS/SSA Subtitles",
  dialog_filter_srt_subtitles: "SRT Subtitles",
  dialog_filter_sub_subtitles: "SUB (MicroDVD)",
  dialog_filter_webvtt: "WebVTT",
  dialog_filter_all_subtitle_formats: "All Subtitle Formats",
  dialog_filter_all_files: "All Files",
  dialog_filter_font_files: "Font Files",
  dialog_filter_video_subtitle_files: "Video & Subtitle Files",
  dialog_filter_video_files: "Video Files",
  dialog_filter_subtitle_files: "Subtitle Files",
  dialog_pick_subtitle_files_title: "Select subtitle files",
  dialog_pick_ass_files_title: "Select ASS/SSA files",
  dialog_pick_rename_inputs_title: "Select videos and subtitles",
  dialog_pick_output_directory_title: "Choose output directory",
  dialog_pick_font_directory_title: "Select font folder",
  dialog_pick_font_files_title: "Select font files",
};

function dt(t: DialogTranslator | undefined, key: string): string {
  return t ? t(key) : (dialogFallbacks[key] ?? key);
}

function subtitleFilters(t?: DialogTranslator): FileFilter[] {
  return [
    { name: dt(t, "dialog_filter_ass_ssa_subtitles"), extensions: ["ass", "ssa"] },
    { name: dt(t, "dialog_filter_srt_subtitles"), extensions: ["srt"] },
    { name: dt(t, "dialog_filter_sub_subtitles"), extensions: ["sub"] },
    { name: dt(t, "dialog_filter_webvtt"), extensions: ["vtt"] },
    {
      name: dt(t, "dialog_filter_all_subtitle_formats"),
      extensions: ["ass", "ssa", "srt", "sub", "vtt", "sbv", "lrc"],
    },
    { name: dt(t, "dialog_filter_all_files"), extensions: ["*"] },
  ];
}

function assFilters(t?: DialogTranslator): FileFilter[] {
  return [
    { name: dt(t, "dialog_filter_ass_ssa_subtitles"), extensions: ["ass", "ssa"] },
    { name: dt(t, "dialog_filter_all_files"), extensions: ["*"] },
  ];
}

function fontFilters(t?: DialogTranslator): FileFilter[] {
  return [
    { name: dt(t, "dialog_filter_font_files"), extensions: ["ttf", "otf", "ttc", "otc"] },
    { name: dt(t, "dialog_filter_all_files"), extensions: ["*"] },
  ];
}

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
export async function pickSubtitleFiles(t?: DialogTranslator): Promise<string[] | null> {
  return toMultiplePaths(
    await open({
      multiple: true,
      filters: subtitleFilters(t),
      title: dt(t, "dialog_pick_subtitle_files_title"),
    })
  );
}

/** Open a multi-file picker for ASS files. Used by Font Embed batch flow,
 *  which only applies to ASS/SSA inputs (other subtitle formats don't
 *  carry font references). */
export async function pickAssFiles(t?: DialogTranslator): Promise<string[] | null> {
  return toMultiplePaths(
    await open({
      multiple: true,
      filters: assFilters(t),
      title: dt(t, "dialog_pick_ass_files_title"),
    })
  );
}

function videoAndSubtitleFilters(t?: DialogTranslator): FileFilter[] {
  return [
    {
      name: dt(t, "dialog_filter_video_subtitle_files"),
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
      name: dt(t, "dialog_filter_video_files"),
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
      name: dt(t, "dialog_filter_subtitle_files"),
      extensions: ["ass", "ssa", "srt", "sub", "vtt", "sbv", "lrc"],
    },
    { name: dt(t, "dialog_filter_all_files"), extensions: ["*"] },
  ];
}

/** Open a multi-file picker accepting both videos and subtitles. Used by
 *  the Batch Rename tab, which auto-categorizes by extension after pick. */
export async function pickRenameInputs(t?: DialogTranslator): Promise<string[] | null> {
  return toMultiplePaths(
    await open({
      multiple: true,
      filters: videoAndSubtitleFilters(t),
      title: dt(t, "dialog_pick_rename_inputs_title"),
    })
  );
}

/** Open a directory picker for the Batch Rename "copy to chosen
 *  directory" output mode. Returns absolute path or null on cancel. */
export async function pickOutputDirectory(t?: DialogTranslator): Promise<string | null> {
  return toSinglePath(
    await open({
      directory: true,
      multiple: false,
      title: dt(t, "dialog_pick_output_directory_title"),
    })
  );
}

/** Open a directory picker for a local font folder. Returns path or null. */
export async function pickFontDirectory(t?: DialogTranslator): Promise<string | null> {
  return toSinglePath(
    await open({
      directory: true,
      multiple: false,
      title: dt(t, "dialog_pick_font_directory_title"),
    })
  );
}

/** Open a multi-file picker for individual font files. Returns paths or null. */
export async function pickFontFiles(t?: DialogTranslator): Promise<string[] | null> {
  return toMultiplePaths(
    await open({
      multiple: true,
      filters: fontFilters(t),
      title: dt(t, "dialog_pick_font_files_title"),
    })
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

/** Streaming progress payload from the Rust scan commands. `Batch` carries
 *  newly-parsed faces; `Done` is the end-of-stream sentinel that signals
 *  every batch has drained. The sentinel exists because Tauri's Channel
 *  splits delivery between sync `webview.eval()` (payloads < 8 KB) and
 *  async fetch (payloads ≥ 8 KB). The invoke promise resolves before the
 *  async batches arrive — without `Done` the UI could report completion
 *  before every progress callback drained. The Channel layer guarantees
 *  in-order delivery, so `Done` only fires after every preceding batch
 *  has been processed. See A-bug-1 in v1.3.1 design doc. */
// Wire-format mirror of Rust's `ScanProgress` enum in
// `src-tauri/src/fonts.rs`, serialized via
// `#[serde(tag = "kind", rename_all = "camelCase")]`. The two type
// definitions are NOT generated from each other; renaming a Rust enum
// variant or adding a field on one side without the other will
// silently break the channel callback (the `if msg.kind === "batch"`
// branch wouldn't match and the frontend hangs awaiting Done). When
// editing one side, edit the other in the same commit.
/** Wire-format mirror of `fonts::ScanStopReason`. Bare lowercased
 *  camelCase strings — units enums in serde serialize this way. Three
 *  legitimate states; see the Rust enum for full semantics.
 *
 *  - `natural`: scan finished walking the entire input.
 *  - `userCancel`: user pressed Cancel mid-scan.
 *  - `ceilingHit`: MAX_FONTS_PER_SCAN defense-in-depth fired (frontend
 *    surfaces "source too large" rather than "cancelled"). */
export type FontScanReason = "natural" | "userCancel" | "ceilingHit";

type RawScanProgress =
  | { kind: "batch"; total: number }
  | {
      kind: "done";
      reason: FontScanReason;
      added: number;
      duplicated: number;
    };

/** Optional callback for streaming font scan results. Called once per
 *  Rust-side batch (cadence determined by `SCAN_BATCH_SIZE` and
 *  `SCAN_BATCH_INTERVAL` in `src-tauri/src/fonts.rs` — currently 40 faces
 *  or 100 ms, whichever fires first). The heavy font-source index stays in
 *  Rust; this callback only exposes the displayed cumulative count. */
export type ScanProgressCallback = (total: number) => void;

export interface FontScanResult {
  added: number;
  duplicated: number;
  /** Why the scan stopped — see `FontScanReason`. Replaces the prior
   *  `(cancelled, ceilingHit)` boolean pair which encoded only three
   *  legitimate states across four flag combinations. */
  reason: FontScanReason;
}

export interface FontScanPreflight {
  fontFiles: number;
  totalBytes: number;
}

export async function preflightFontDirectory(dir: string): Promise<FontScanPreflight> {
  return invoke<FontScanPreflight>("preflight_font_directory", { dir });
}

export async function preflightFontFiles(paths: string[]): Promise<FontScanPreflight> {
  return invoke<FontScanPreflight>("preflight_font_files", { paths });
}

/**
 * Scan a user-picked directory (one level deep) for font files. Rust keeps
 * the heavy source index; the frontend receives progress counts plus how
 * many faces were registered after dedup. TTC files may contribute multiple
 * faces sharing the same path.
 *
 * Cancellation: call {@link cancelFontScan} from a button handler. The
 * Rust scan returns early; the resolved result reports the partial set
 * registered up to that point (no rejection — partial preservation is the
 * contract).
 */
export async function scanFontDirectory(
  dir: string,
  sourceId: string,
  scanId: number,
  onBatch?: ScanProgressCallback
): Promise<FontScanResult> {
  return runStreamingScan("scan_font_directory", { dir, sourceId, scanId }, onBatch);
}

/** Scan a user-supplied list of individual font file paths. Same streaming
 *  contract as {@link scanFontDirectory}. */
export async function scanFontFiles(
  paths: string[],
  sourceId: string,
  scanId: number,
  onBatch?: ScanProgressCallback
): Promise<FontScanResult> {
  return runStreamingScan("scan_font_files", { paths, sourceId, scanId }, onBatch);
}

/** Request the current font scan be cancelled. Idempotent — safe to call
 *  even when no scan is active. The running scan returns its partial list
 *  via the same Promise the caller is awaiting. */
export async function cancelFontScan(scanId: number): Promise<void> {
  await invoke("cancel_font_scan", { scanId });
}

export async function resolveUserFont(
  family: string,
  bold: boolean,
  italic: boolean
): Promise<FontLookupResult | null> {
  return invoke<FontLookupResult | null>("resolve_user_font", { family, bold, italic });
}

export async function removeFontSource(sourceId: string): Promise<void> {
  await invoke("remove_font_source", { sourceId });
}

export async function clearFontSources(): Promise<void> {
  await invoke("clear_font_sources");
}

/** Shared streaming-invoke wrapper for both scan commands. Constructs a
 *  Channel<ScanProgress>, waits for Done, and resolves with the Rust-side
 *  registration counts and cancellation outcome. */
async function runStreamingScan(
  command: "scan_font_directory" | "scan_font_files",
  args: Record<string, unknown>,
  onBatch?: ScanProgressCallback
): Promise<FontScanResult> {
  const channel = new Channel<RawScanProgress>();
  // Resolved by the `Done` handler. Awaited after invoke so the function
  // returns only once every preceding `Batch` (sync OR async) has fired.
  // Definite-assignment assertion: the Promise constructor calls its
  // executor synchronously, so `resolveDone` is set before any consumer
  // (channel.onmessage / await donePromise) could possibly read it.
  // No `?.` needed at the call site.
  let resolveDone!: () => void;
  const donePromise = new Promise<void>((resolve) => {
    resolveDone = resolve;
  });
  const result: FontScanResult = {
    added: 0,
    duplicated: 0,
    reason: "natural",
  };
  channel.onmessage = (msg) => {
    if (msg.kind === "batch") {
      onBatch?.(msg.total);
    } else if (msg.kind === "done") {
      result.reason = msg.reason;
      result.added = msg.added;
      result.duplicated = msg.duplicated;
      resolveDone();
    } else {
      // Defense-in-depth: TypeScript narrows the union exhaustively at
      // compile time, but a Rust enum variant rename without updating
      // RawScanProgress would silently fall through here. Surface in
      // dev so future drift is visible. Guard the cast — a future
      // Rust-side serde change to a non-object payload (untagged enum,
      // bare value) would otherwise throw on `.kind` access here.
      const tag =
        typeof msg === "object" && msg !== null && "kind" in msg
          ? (msg as { kind: unknown }).kind
          : msg;
      console.warn("unknown ScanProgress payload:", tag);
    }
  };
  await invoke(command, { ...args, progress: channel });
  // Rust always emits Done on the Ok path. Channel guarantees in-order
  // delivery of Batch+Done, so awaiting Done forces every async-fetched
  // progress event to drain before we report the final counts.
  await donePromise;
  return result;
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

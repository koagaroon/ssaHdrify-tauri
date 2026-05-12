/**
 * Thin wrappers around Tauri IPC for file I/O and dialogs.
 * Centralizes all native interactions so feature code stays pure JS.
 */
import { open } from "@tauri-apps/plugin-dialog";
import { invoke, Channel } from "@tauri-apps/api/core";
import { Base64 } from "js-base64";

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

/** Write a text file with explicit UTF-8.
 *
 *  Routes through the Rust-side `safe_write_text_file` command (not
 *  `@tauri-apps/plugin-fs` writeTextFile, which follows reparse points
 *  and would happily write through a planted symlink at the output
 *  path — see Codex finding 776ff6ef / commit b7d9d21 + the safe_io
 *  module doc). The command refuses if the destination already exists
 *  as a symlink / junction, regardless of the overwrite flag; for
 *  regular-file destinations it removes the file first and re-creates
 *  via `OpenOptions::create_new(true)` for an atomic guard.
 *
 *  Overwrite is hardcoded to true here: every callsite preflights
 *  collisions via `countExistingFiles` in `src/lib/output-collisions.ts`
 *  and asks the user before invoking writeText. */
export async function writeText(path: string, content: string): Promise<void> {
  await invoke("safe_write_text_file", { path, content, overwrite: true });
}

/** Rename / move a file. Atomic on the same volume; cross-volume falls
 *  back to copy-then-delete (std::fs::rename semantics). Used by Batch
 *  Rename's "rename in place" mode where the source file disappears.
 *
 *  Routes through `safe_rename_file` rather than `@tauri-apps/plugin-fs`
 *  rename, which would let a symlinked source rename a sensitive target
 *  or let a symlinked destination redirect the move outside the user-
 *  selected output dir (Codex findings 818eb84f / d29ac141). The
 *  command refuses if either endpoint is a reparse point. */
export async function renamePath(from: string, to: string): Promise<void> {
  await invoke("safe_rename_file", { src: from, dst: to, overwrite: true });
}

/** Copy a file. Source is preserved. Used by Batch Rename's two copy
 *  modes (copy-to-video-directory / copy-to-chosen). Overwrites the
 *  target if it exists — pre-flight overwrite confirmation lives at
 *  the caller.
 *
 *  Routes through `safe_copy_file` for the same reason as renamePath:
 *  the plugin-fs copyFile follows symlinks on both endpoints, which a
 *  malicious or accidental shortcut in a downloaded fan-sub pack can
 *  abuse to read a sensitive source (e.g. `~/.ssh/id_rsa`) and copy
 *  it into the user's video directory under a subtitle-looking name. */
export async function copyPath(from: string, to: string): Promise<void> {
  await invoke("safe_copy_file", { src: from, dst: to, overwrite: true });
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

/** Subset a font file to only include the specified codepoints.
 *
 *  Wire format: Rust returns the bytes base64-encoded (`subset_font_b64`)
 *  to dodge the JSON `[byte, byte, ...]` form's ~4–5× expansion. The
 *  worst-case 10 MB fallback subset would otherwise produce a ~50 MB
 *  IPC payload + main-thread JSON parse pass; base64 is ~1.33× and
 *  `atob` is V8 builtin. Mirrors chain-runtime's `decodeBase64`. */
export async function subsetFont(
  fontPath: string,
  fontIndex: number,
  codepoints: number[]
): Promise<Uint8Array> {
  const b64: string = await invoke("subset_font_b64", { fontPath, fontIndex, codepoints });
  // js-base64 (consistent with chain-runtime's decodeBase64). WebView2
  // has a working `atob`, but using one decoder library across both
  // paths keeps the encoding contract single-rooted — a future bug fix
  // in one site shows up in the other automatically (Round 1 F1.N-R1-15).
  const bytes = Base64.toUint8Array(b64);
  return bytes;
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

/** Remove a font source from the session index. `kind` ("dir" or "files")
 *  determines whether the persistent cache eviction runs — only dir-mode
 *  sources populated the cache in the first place, so files-mode removals
 *  must skip eviction to avoid wrongly evicting a coincident dir source
 *  whose folder shares a parent with the removed file (Codex 3d751e26). */
export async function removeFontSource(sourceId: string, kind: "dir" | "files"): Promise<void> {
  await invoke("remove_font_source", { sourceId, kind });
}

export async function clearFontSources(): Promise<void> {
  await invoke("clear_font_sources");
}

// ---- Persistent font cache (#5) ----------------------------------------
// Wraps src-tauri/src/font_cache_commands.rs. Field names are camelCase
// because Rust serializes with #[serde(rename_all = "camelCase")] on the
// status / rescan-result types; DriftReport uses snake_case fields
// (added/modified/removed) which happen to be camelCase-equivalent.

/** Status of the GUI persistent font cache. Returned by openFontCache. */
export interface FontCacheStatus {
  /** True when the cache file is loaded and lookups will work. */
  available: boolean;
  /** True when the file on disk has a schema version different from this build.
   *  Mutually exclusive with `available`. The drift modal renders a "rebuild
   *  required" path in this state — `clearFontCache` is the recovery action. */
  schemaMismatch: boolean;
  /** Absolute path to the cache file. Always populated post-init. */
  path: string;
}

/** Drift between cache and live filesystem. `added` is always empty in the
 *  current GUI flow (we don't walk source roots from this command). */
export interface FontCacheDriftReport {
  added: string[];
  modified: string[];
  removed: string[];
}

/** One folder that didn't make it through a clean rescan. `kind`
 *  distinguishes Phase-2 scan failure (couldn't read the folder) from
 *  Phase-3 apply failure (couldn't write the cache row). The modal
 *  renders both kinds in the same partial-success block (Round 3
 *  N-R3-2). */
export type FontCacheSkipKind = "scanFailed" | "applyFailed";

export interface FontCacheSkippedFolder {
  folder: string;
  reason: string;
  kind: FontCacheSkipKind;
}

/** Outcome of rescanFontCacheDrift. `skipped` is non-empty when Phase 2
 *  scan errors out for some folders — the stale rows have been evicted
 *  (so font lookups fall through cleanly) but the user should know which
 *  folders need attention. */
export interface FontCacheRescanResult {
  modifiedRescanned: number;
  removedEvicted: number;
  skipped: FontCacheSkippedFolder[];
}

/** Probe the cache state. Idempotent; safe to call multiple times. */
export async function openFontCache(): Promise<FontCacheStatus> {
  return invoke<FontCacheStatus>("open_font_cache");
}

/** Detect drift between cached folders and live filesystem. */
export async function detectFontCacheDrift(): Promise<FontCacheDriftReport> {
  return invoke<FontCacheDriftReport>("detect_font_cache_drift");
}

/** Bring the cache back into sync: re-scan modified folders, evict removed
 *  ones. May take a while on large libraries — show a spinner. */
export async function rescanFontCacheDrift(): Promise<FontCacheRescanResult> {
  return invoke<FontCacheRescanResult>("rescan_font_cache_drift");
}

/** Wipe the cache file and re-create an empty one with current schema.
 *  Used as the modal's "Clear cache" button and as the recovery path
 *  for `schemaMismatch`. */
export async function clearFontCache(): Promise<void> {
  await invoke("clear_font_cache");
}

/** Look up a (family, bold, italic) tuple in the cache. Returns null when
 *  not found OR when the cache is unavailable — the embed pipeline treats
 *  both the same (fall through to the next resolution tier). */
export async function lookupFontFamily(
  family: string,
  bold: boolean,
  italic: boolean
): Promise<FontLookupResult | null> {
  return invoke<FontLookupResult | null>("lookup_font_family", { family, bold, italic });
}

/** Shared streaming-invoke wrapper for both scan commands. Constructs a
 *  Channel<ScanProgress>, waits for Done, and resolves with the Rust-side
 *  registration counts and cancellation outcome.
 *
 *  `onBatch` callbacks are PROVISIONAL — they reflect what the Rust scan
 *  worker has emitted so far, NOT what has been committed to the SQLite
 *  user-font index. If the import transaction rolls back (SQLite BUSY,
 *  schema constraint violation), zero fonts are registered and the
 *  invoke() call rejects. Callers that surface batch counts in the UI
 *  must clear them on rejection so the display doesn't claim partial
 *  registration when none happened — this wrapper does that via the
 *  catch path below.
 *
 *  No JS-side timeout by design. The Rust scan worker is bounded by
 *  MAX_FONTS_PER_SCAN (defense-in-depth ceiling) and the user always
 *  has the inline Cancel button (cancelFontScan) to abort. Adding a
 *  timeout here would race legitimate slow scans (XL font collection
 *  on a slow disk can take 30+ seconds) and produce false-cancel
 *  signals the Rust side never sent. If a future stuck-IPC failure
 *  mode emerges, a watchdog should live in the Tauri command layer
 *  (Rust-side timeout on spawn_blocking), not here. */
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
  // Guard against a duplicate `Done` event silently overwriting the
  // first one's counts. The Rust contract emits exactly one Done on the
  // Ok path; a future encoder bug or refactor that violates that would
  // otherwise corrupt `result` last-wins style with no signal.
  let doneReceived = false;
  channel.onmessage = (msg) => {
    if (msg.kind === "batch") {
      onBatch?.(msg.total);
    } else if (msg.kind === "done") {
      if (doneReceived) {
        console.warn("Duplicate scan Done event received; ignoring");
        return;
      }
      doneReceived = true;
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
  try {
    await invoke(command, { ...args, progress: channel });
  } catch (err) {
    // Rust returned Err — no Done was emitted, so `donePromise` is still
    // unresolved. Reset the provisional batch count UNCONDITIONALLY so
    // any caller showing "Scanned N fonts" doesn't sit alongside the
    // error message implying partial registration; the import
    // transaction has rolled back and zero fonts were committed. Callers
    // must treat onBatch(0) as a "reset signal" not just a count update.
    // Then detach the listener — Tauri drops the channel sender on Err
    // return today, but a late event slipping through (lifecycle bug,
    // async-fetch path quirk) would otherwise call onBatch after the
    // catch already resolved the UI to the error state, producing a
    // confusing 5, 7, 0, 12 sequence.
    //
    // Intentional: do NOT resolve `donePromise` here. The rejection
    // propagates out of this function before the `await donePromise`
    // line below, which is correct. A future refactor wrapping this
    // invoke in retry logic without explicitly rejecting/resolving
    // donePromise on the no-retry-left branch would cause a permanent
    // hang.
    onBatch?.(0);
    channel.onmessage = () => {};
    throw err;
  }
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

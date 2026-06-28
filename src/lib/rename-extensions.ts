/**
 * Shared file-extension classification for file pickers and workflow
 * input buckets.
 *
 * Text subtitle workflows and Batch Rename intentionally do NOT share
 * one subtitle extension set. Timing / HDR / text reads can only accept
 * formats the parser understands; Batch Rename can also move opaque
 * sidecars such as Blu-ray PGS `.sup` without reading them as text.
 *
 * Consolidated here because the previous "two sides is small enough"
 * comment in BatchRename.tsx assumed N=2; the CLI engine made it N=3
 * + picker = N=4 — past the threshold where the shared module pays
 * for its import cost.
 *
 * Categories rationale:
 * - VIDEO_EXTS: container formats we route into the video bucket. Order
 *   inside the Set has no semantic effect.
 * - SUBTITLE_EXTS: text-subtitle formats. Same set is used by the
 *   TimingShift "is this a subtitle file?" check.
 * - RENAME_SUBTITLE_EXTS: subtitle sidecars Batch Rename may pair,
 *   copy, or rename without parsing. Keep this broader set out of
 *   Timing Shift / HDR / readText gates.
 * - IGNORED_EXTS: companion files that ship in fan-sub release folders
 *   but have no place in this app's workflow. Surfacing them as
 *   "unknown" would be noise. Sub-categories:
 *     - source / metadata        : torrent
 *     - common archive formats   : zip, rar, 7z, tar, gz, bz2, xz, tgz
 *     - companion audio tracks   : mka, flac, mp3, m4a, aac (separate
 *                                  audio supplied alongside HEVC video)
 *   Add to IGNORED_EXTS when a release-folder staple appears that can
 *   never be a Tab 4 input.
 */

export const VIDEO_EXTS: ReadonlySet<string> = new Set([
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
  "ogv",
  "rmvb",
]);

// `sbv` (SubViewer) and `lrc` (LRC lyrics) are intentionally omitted —
// `subtitle-parser.ts::detectFormat` only recognizes `ass | ssa | srt |
// sub | vtt`, so the previous superset let .sbv/.lrc files through the
// TimingShift folder-drop filter (which uses `categorize(name) ===
// "subtitle"`) only to fail with a confusing "Could not detect subtitle
// format" inside the parser. The two sets disagreed on "is this a
// subtitle file" — the extension bucket and parser must stay coherent.
// ssaHdrify's target fan-sub workflow doesn't touch .sbv or
// .lrc (SubViewer is a legacy format mostly seen on older platforms;
// LRC is karaoke-style lyrics, not subtitles), so the cleanest fix is
// to narrow the SUBTITLE_EXTS set to what the parser actually handles.
// BatchRename's pairing logic also tightens accordingly — files with
// those extensions now route to "unknown" bucket and surface in the
// drop summary, which is the right outcome since pairing them
// wouldn't lead to a usable output.
export const SUBTITLE_EXTS: ReadonlySet<string> = new Set(["ass", "ssa", "srt", "sub", "vtt"]);

export const RENAME_SUBTITLE_EXTS: ReadonlySet<string> = new Set([...SUBTITLE_EXTS, "sup"]);

export const IGNORED_EXTS: ReadonlySet<string> = new Set([
  "torrent",
  "zip",
  "rar",
  "7z",
  "tar",
  "gz",
  "bz2",
  "xz",
  "tgz",
  "mka",
  "flac",
  "mp3",
  "m4a",
  "aac",
]);

export type RenameCategory = "video" | "subtitle" | "ignored" | "unknown";

export function categorize(name: string): RenameCategory {
  return categorizeWithSubtitleSet(name, SUBTITLE_EXTS);
}

export function categorizeForRename(name: string): RenameCategory {
  return categorizeWithSubtitleSet(name, RENAME_SUBTITLE_EXTS);
}

function categorizeWithSubtitleSet(
  name: string,
  subtitleExts: ReadonlySet<string>
): RenameCategory {
  const dot = name.lastIndexOf(".");
  if (dot < 0) return "unknown";
  const ext = name.slice(dot + 1).toLowerCase();
  if (VIDEO_EXTS.has(ext)) return "video";
  if (subtitleExts.has(ext)) return "subtitle";
  if (IGNORED_EXTS.has(ext)) return "ignored";
  return "unknown";
}

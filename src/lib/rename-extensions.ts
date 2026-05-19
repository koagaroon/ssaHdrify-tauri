/**
 * Shared file-extension classification for the Batch Rename feature.
 *
 * Three Sets + a `categorize(name)` helper drive both the post-drop /
 * post-pick classification (CLI engine + GUI BatchRename) AND the open-
 * dialog picker filter (tauri-api.ts). Before this extraction the three
 * Sets were defined verbatim across two files and the picker filter
 * carried a fourth inline copy — a future ext-set change (`.av1`, `.heic`,
 * etc.) would have needed to land in four places.
 *
 * R2 N-R2-2 / N-R2-3: consolidated. The previous "two sides is small
 * enough" comment in BatchRename.tsx assumed N=2; the CLI engine made it
 * N=3 + picker = N=4 — past the threshold where the shared module pays
 * for its import cost.
 *
 * Categories rationale:
 * - VIDEO_EXTS: container formats we route into the video bucket. Order
 *   inside the Set has no semantic effect.
 * - SUBTITLE_EXTS: text-subtitle formats. Same set is used by the
 *   TimingShift "is this a subtitle file?" check.
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

export const SUBTITLE_EXTS: ReadonlySet<string> = new Set([
  "ass",
  "ssa",
  "srt",
  "sub",
  "vtt",
  "sbv",
  "lrc",
]);

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
  const dot = name.lastIndexOf(".");
  if (dot < 0) return "unknown";
  const ext = name.slice(dot + 1).toLowerCase();
  if (VIDEO_EXTS.has(ext)) return "video";
  if (SUBTITLE_EXTS.has(ext)) return "subtitle";
  if (IGNORED_EXTS.has(ext)) return "ignored";
  return "unknown";
}

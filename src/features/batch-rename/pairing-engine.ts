/**
 * Pairing engine — fan-sub style episode + season extraction and
 * video↔subtitle pairing for Tab 4 (Batch Rename).
 *
 * Algorithm (per design doc 已决定 #1):
 *   1. bracket cleanup           strip every [..] group
 *   2. priority-ordered episode  regex set (first match wins)
 *   3. season parallel scan      (only when ep regex didn't carry it)
 *   4. pair by (season, episode) tuple
 *   5. LCS fallback              [NOT in Stage 5b — regex covers all
 *                                 7 documented fan-sub samples; LCS
 *                                 lands when a real failure surfaces]
 *
 * Pattern coverage: validated against LoliHouse / Haruhana / Airota /
 * Nekomoe kissaten / 樱桃花字幕组 / DBD-Raws naming. The original
 * Western-TV regex set (S\dE\d / EP\d / 第N话) hit zero of seven
 * samples; this set hits all seven via Pattern A (` - NN [...]`) and
 * Pattern B (`][NN][`), with the original set kept as fallback.
 */

// ── Bracket cleanup ──────────────────────────────────────

const BRACKET_RE = /\[[^\]]*\]/g;

/** Strip every `[...]` group from a filename and collapse whitespace.
 *  Used as a separate cleaning pass for season-scan / LCS regexes that
 *  don't depend on the bracket structure (vs. Pattern B which keys
 *  off the brackets and runs on the raw filename). */
export function bracketCleanup(filename: string): string {
  return filename.replace(BRACKET_RE, " ").replace(/\s+/g, " ").trim();
}

// ── Episode extraction ──────────────────────────────────

export interface EpisodeResult {
  episode: number;
  /** Set only when the regex captured both season and episode (e.g.
   *  `S01E01`). Otherwise the season is filled in by extractSeason. */
  seasonFromMatch?: number;
}

/** Episode-extraction patterns, priority order. The boolean `useRaw`
 *  decides whether the pattern runs on the raw filename (with brackets)
 *  or the bracket-cleaned form. */
interface EpisodePattern {
  regex: RegExp;
  useRaw: boolean;
  build: (m: RegExpMatchArray) => EpisodeResult;
}

const EPISODE_PATTERNS: EpisodePattern[] = [
  // Western S01E01 — both season and episode, highest confidence.
  // Matches on either raw or cleaned (no bracket dependency).
  {
    regex: /\bS(\d+)E(\d+)\b/i,
    useRaw: true,
    build: (m) => ({
      episode: parseInt(m[2], 10),
      seasonFromMatch: parseInt(m[1], 10),
    }),
  },
  // Pattern B — `][NN][` — must run on raw (brackets are the cue).
  // Airota / Nekomoe-style fan-sub naming.
  {
    regex: /\]\s*\[\s*0*(\d+)\s*\]/,
    useRaw: true,
    build: (m) => ({ episode: parseInt(m[1], 10) }),
  },
  // Pattern A — ` - NN [` or ` - NN.ext` — runs on raw because the
  // trailing bracket / extension boundary is the right anchor. Most
  // common format across the documented samples (LoliHouse, Haruhana,
  // Nekomoe&LoliHouse, 樱桃花字幕组).
  {
    regex: /\s-\s*0*(\d+)\s*(?:\[|\.[a-z0-9]+$)/i,
    useRaw: true,
    build: (m) => ({ episode: parseInt(m[1], 10) }),
  },
  // 第N话 / 第N集 — Chinese marker, fallback. Doesn't appear in the
  // documented corpus but worth keeping for older naming styles.
  {
    regex: /第\s*(\d+)\s*[话集]/,
    useRaw: false,
    build: (m) => ({ episode: parseInt(m[1], 10) }),
  },
  // Western EP01 / E01 — final fallback.
  {
    regex: /\bEP?(\d+)\b/i,
    useRaw: false,
    build: (m) => ({ episode: parseInt(m[1], 10) }),
  },
];

export function extractEpisode(rawName: string, cleanedName: string): EpisodeResult | null {
  for (const { regex, useRaw, build } of EPISODE_PATTERNS) {
    const target = useRaw ? rawName : cleanedName;
    const m = target.match(regex);
    if (m) return build(m);
  }
  return null;
}

// ── Season parallel scan ────────────────────────────────

/** Convert a 1–99 Chinese numeral string to integer. Returns 1 for
 *  unrecognized input — callers treat that as "default season 1". */
function chineseNumeralToInt(s: string): number {
  const map: Record<string, number> = {
    一: 1,
    二: 2,
    三: 3,
    四: 4,
    五: 5,
    六: 6,
    七: 7,
    八: 8,
    九: 9,
    十: 10,
  };
  if (s.length === 1) return map[s] ?? 1;
  if (s === "十") return 10;
  const tenIdx = s.indexOf("十");
  if (tenIdx === -1) return 1;
  // Forms: 十N (10..19), N十 (20, 30...), N十M (21..99).
  const tens = tenIdx === 0 ? 1 : (map[s[0]] ?? 1);
  const ones = tenIdx === s.length - 1 ? 0 : (map[s[tenIdx + 1]] ?? 0);
  return tens * 10 + ones;
}

interface SeasonPattern {
  regex: RegExp;
  build: (m: RegExpMatchArray) => number;
}

const SEASON_PATTERNS: SeasonPattern[] = [
  // "Nnd Season" / "Nrd Season" / "Nth Season" — anime fan-sub style.
  {
    regex: /(\d+)(?:st|nd|rd|th)\s+Season/i,
    build: (m) => parseInt(m[1], 10),
  },
  // "Season N"
  {
    regex: /Season\s+(\d+)/i,
    build: (m) => parseInt(m[1], 10),
  },
  // 第N季 — Chinese ordinal + numeric or 一二三...
  {
    regex: /第\s*([一二三四五六七八九十\d]+)\s*季/,
    build: (m) => {
      const raw = m[1];
      const n = parseInt(raw, 10);
      if (!Number.isNaN(n)) return n;
      return chineseNumeralToInt(raw);
    },
  },
  // Standalone S\d (negative lookahead so "S01E01" doesn't double-count).
  {
    regex: /\bS(\d+)(?!E\d+)\b/i,
    build: (m) => parseInt(m[1], 10),
  },
];

export function extractSeason(rawName: string, cleanedName: string): number {
  // Priority-ordered scan on RAW so brackets-near-season cues are
  // preserved; standalone S\d is also valid on cleaned but raw works.
  for (const { regex, build } of SEASON_PATTERNS) {
    const m = rawName.match(regex) ?? cleanedName.match(regex);
    if (m) {
      const n = build(m);
      if (n > 0) return n;
    }
  }
  return 1;
}

// ── Parsed file ─────────────────────────────────────────

export interface ParsedFile {
  path: string;
  name: string;
  cleaned: string;
  season: number;
  episode: number | null;
}

export function parseFilename(path: string, name: string): ParsedFile {
  // Drop the extension before parsing — extensions like ".ass" / ".mkv"
  // can otherwise be mistaken for episode markers under the EP\d
  // fallback. Pattern A's `.[a-z]+$` anchor still matches because we
  // give it the original raw name; only the internal `cleaned` form
  // strips the extension.
  const rawForRegex = name;
  const cleaned = bracketCleanup(stripExtension(name));
  const ep = extractEpisode(rawForRegex, cleaned);
  const season = ep?.seasonFromMatch ?? extractSeason(rawForRegex, cleaned);
  return {
    path,
    name,
    cleaned,
    season,
    episode: ep?.episode ?? null,
  };
}

function stripExtension(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(0, dot) : name;
}

// ── Pairing rows ────────────────────────────────────────

export type PairingSource = "regex" | "lcs" | "manual" | "unmatched" | "warning";

export interface PairingRow {
  /** Stable ID for React reconciliation. Survives reorder. */
  id: string;
  video: { path: string; name: string } | null;
  subtitle: { path: string; name: string } | null;
  source: PairingSource;
  /** Default selection: first (video, sub) pair per video gets true,
   *  additional rows for the same video get false. Stage 5c will let
   *  the user toggle this. Rows without both video + sub default to
   *  false (nothing to do at output). */
  selected: boolean;
  /** Pairing key — `${season}|${episode}` for matched, `unmatched` for
   *  files where the regex set found no episode. Used for sorting and
   *  for grouping multi-language subs under one video. */
  key: string;
}

function pairingKeyTuple(season: number, episode: number): string {
  return `${season}|${episode}`;
}

/** Compose a stable row ID from the file paths. Survives reorders and
 *  reanalysis as long as the same (video, subtitle) pair is still
 *  produced — required so user-driven per-row overrides (Stage 5c
 *  selection toggle) don't get orphaned when files come and go. */
export function makeRowId(
  videoPath: string | null | undefined,
  subtitlePath: string | null | undefined
): string {
  return `${videoPath ?? "_"}|||${subtitlePath ?? "_"}`;
}

/**
 * Build pairing rows from the input file lists. Video-centric:
 *
 *   - Exactly one row per video. Subtitles are a property of the row,
 *     not their own row. Multi-language batches (e.g., DBD-Raws's
 *     `.sc.ass` + `.tc.ass` per video) get the first regex-paired sub
 *     selected by default; the others stay reachable through the UI's
 *     subtitle dropdown without inflating the grid.
 *   - Subtitles whose episode regex didn't match any video are NOT
 *     given their own row. They remain in the input subtitle list and
 *     are still selectable via any video row's dropdown — the user is
 *     looking for a sub for a video, not the other way around.
 *   - Ambiguous case (multiple videos share `(season, episode)`):
 *     each video gets its own row, all marked `warning`. Default
 *     pre-pairs by index so the user sees a defensible default.
 *   - Orphan video (no matching sub): one row, subtitle null, source
 *     `unmatched`. Selected = false until the user picks a sub.
 *
 * Rows are sorted by `(season, episode)`; unmatched videos go at the
 * bottom in their input order.
 */
export function buildPairings(videos: ParsedFile[], subtitles: ParsedFile[]): PairingRow[] {
  const rows: PairingRow[] = [];
  const newId = (v: ParsedFile, s: ParsedFile | null) => makeRowId(v.path, s?.path);

  // Bucket matched subs by key. Unmatched subs are intentionally
  // dropped from the row set per the video-centric model — they
  // stay accessible through the UI's per-row dropdown.
  const matchedSubs = new Map<string, ParsedFile[]>();
  for (const s of subtitles) {
    if (s.episode === null) continue;
    const key = pairingKeyTuple(s.season, s.episode);
    const arr = matchedSubs.get(key) ?? [];
    arr.push(s);
    matchedSubs.set(key, arr);
  }

  // Bucket videos by key (preserving order within each key) plus a
  // tail bucket for videos whose regex didn't match.
  const matchedVideos = new Map<string, ParsedFile[]>();
  const unmatchedVideos: ParsedFile[] = [];
  for (const v of videos) {
    if (v.episode === null) {
      unmatchedVideos.push(v);
      continue;
    }
    const key = pairingKeyTuple(v.season, v.episode);
    const arr = matchedVideos.get(key) ?? [];
    arr.push(v);
    matchedVideos.set(key, arr);
  }

  const sortedKeys = Array.from(matchedVideos.keys()).sort(compareKeys);

  for (const key of sortedKeys) {
    const vs = matchedVideos.get(key) ?? [];
    const ss = matchedSubs.get(key) ?? [];
    const ambiguous = vs.length > 1;

    for (let i = 0; i < vs.length; i++) {
      const v = vs[i];
      // Index-pair videos to subs in the ambiguous case so the user
      // sees a defensible default pre-pick. In the common one-video
      // case, the first sub is always the chosen one.
      const sub = ambiguous ? (ss[i] ?? null) : (ss[0] ?? null);
      rows.push({
        id: newId(v, sub),
        video: { path: v.path, name: v.name },
        subtitle: sub ? { path: sub.path, name: sub.name } : null,
        source: ambiguous ? "warning" : sub ? "regex" : "unmatched",
        selected: sub !== null,
        key,
      });
    }
  }

  for (const v of unmatchedVideos) {
    rows.push({
      id: newId(v, null),
      video: { path: v.path, name: v.name },
      subtitle: null,
      source: "unmatched",
      selected: false,
      key: "unmatched",
    });
  }

  return rows;
}

function compareKeys(a: string, b: string): number {
  const [as, ae] = a.split("|").map((n) => parseInt(n, 10));
  const [bs, be] = b.split("|").map((n) => parseInt(n, 10));
  if (as !== bs) return as - bs;
  return ae - be;
}

// ── Output path derivation ──────────────────────────────

export type OutputMode = "rename" | "copy_to_video" | "copy_to_chosen";

/** Derive the output path for renaming a subtitle to match a video.
 *
 * Output filename = `<video_basename><sub_extension>` — the subtitle
 * basename is replaced with the video basename verbatim (no lang
 * suffix preservation). User intent is exact basename match so a
 * media player auto-loads the sub; modern players already pick up
 * `.zh.ass` / `.sc.ass`-suffixed siblings, so preserving the lang
 * tag would defeat the rename's whole purpose. When the user wants
 * multiple language subs for the same video, the grid checkbox
 * picks ONE per video; collisions on additional checked rows are
 * caught by within-batch dedup.
 *
 * Target directory varies by mode:
 *   - rename          : same directory as the SUBTITLE (source disappears)
 *   - copy_to_video   : same directory as the VIDEO (source preserved)
 *   - copy_to_chosen  : caller-provided directory (source preserved)
 *
 * Native path separator is preserved by inspecting the subtitle path
 * (the canonical "source" for the file we're operating on). Mixing
 * `/` and `\` confuses Win32 APIs and shell-integration tools.
 */
export function deriveRenameOutputPath(
  videoPath: string,
  subtitlePath: string,
  mode: OutputMode,
  chosenDir: string | null
): string {
  // Video basename (without extension)
  const videoBaseFull = baseName(videoPath);
  const videoBase = stripExtension(videoBaseFull);

  // Keep only the subtitle's file extension; the rest of the sub name
  // (including any `.zh` / `.sc` / `.tc` lang token) is discarded so
  // the output basename equals the video basename verbatim.
  const subFull = baseName(subtitlePath);
  const subDot = subFull.lastIndexOf(".");
  const subExt = subDot > 0 ? subFull.slice(subDot) : ""; // ".ass" / ".srt" / etc.

  const outName = `${videoBase}${subExt}`;

  // Pick target directory per mode
  let targetDir: string;
  if (mode === "rename") {
    targetDir = dirname(subtitlePath);
  } else if (mode === "copy_to_video") {
    targetDir = dirname(videoPath);
  } else {
    if (!chosenDir) {
      throw new Error("deriveRenameOutputPath: chosenDir required for copy_to_chosen");
    }
    targetDir = chosenDir;
  }

  // Preserve native path separator from the subtitle path
  const usedBackslash = subtitlePath.includes("\\") && !subtitlePath.includes("/");
  const normTargetDir = targetDir.replace(/\\/g, "/").replace(/\/$/, "");
  const outputPath = normTargetDir ? `${normTargetDir}/${outName}` : outName;
  return usedBackslash ? outputPath.replace(/\//g, "\\") : outputPath;
}

// ── Manual edit ─────────────────────────────────────────

/** Assign a subtitle (or null to unpair) to a target row. The single
 *  manual-edit primitive in the video-centric grid:
 *
 *    - Target row's subtitle becomes `sub`. Source flips to `manual`
 *      so the user sees which rows they've touched.
 *    - If `sub` is non-null and was previously paired with another
 *      row, that other row becomes `(video, null)` source `manual`.
 *      A subtitle is uniquely owned — same path can't appear in two
 *      rows, since both would rename to the same target name.
 *    - Picking the row's current subtitle again is a no-op.
 *    - Picking a subtitle that has no metadata (caller couldn't find
 *      it in the batch's subtitle pool) is a defensive no-op.
 *
 *  The caller passes `sub` as a `{ path, name }` object built from
 *  the batch's subtitle list (not from existing rows), so subs that
 *  aren't currently paired with any row are still selectable. */
export function assignSubtitleToRow(
  rows: PairingRow[],
  targetRowId: string,
  sub: { path: string; name: string } | null
): PairingRow[] {
  const target = rows.find((r) => r.id === targetRowId);
  if (!target) return rows;

  // No-op when the row already has exactly this subtitle (or both
  // null). Compare by path because object identity may differ.
  const currentPath = target.subtitle?.path ?? null;
  const newPath = sub?.path ?? null;
  if (currentPath === newPath) return rows;

  return rows.map((r) => {
    if (r.id === targetRowId) {
      return { ...r, subtitle: sub, source: "manual" as PairingSource };
    }
    if (sub && r.subtitle?.path === sub.path) {
      // Same sub was paired with another row — unpair it there so
      // the sub stays uniquely owned.
      return { ...r, subtitle: null, source: "manual" as PairingSource };
    }
    return r;
  });
}

/** Path equality test for the rename pre-flight no-op detector.
 *  Two paths are "the same file" for our purposes when, after NFC
 *  normalization, slash-style folding, and case folding, they are
 *  identical strings. The fix this helper supports: a DBD-Raws-style
 *  release where the subtitle is already correctly named for the
 *  paired video — running rename/copy on it has no effect (or fails,
 *  for copyFile(src, src) on Windows) and must be filtered out before
 *  the overwrite-confirm dialog so the user doesn't see a spurious
 *  warning. Case folding is OK on Windows (case-insensitive FS) and
 *  acceptable on Linux/macOS too — file-management UI on those
 *  platforms typically discourages case-only renames anyway. */
export function isNoOpRename(subtitlePath: string, outputPath: string): boolean {
  const norm = (p: string) => p.normalize("NFC").replace(/\\/g, "/").toLowerCase();
  return norm(subtitlePath) === norm(outputPath);
}

function baseName(path: string): string {
  const norm = path.replace(/\\/g, "/");
  const lastSlash = norm.lastIndexOf("/");
  return lastSlash >= 0 ? norm.slice(lastSlash + 1) : norm;
}

function dirname(path: string): string {
  const norm = path.replace(/\\/g, "/");
  const lastSlash = norm.lastIndexOf("/");
  if (lastSlash < 0) return "";
  const usedBackslash = path.includes("\\") && !path.includes("/");
  const dir = norm.slice(0, lastSlash);
  return usedBackslash ? dir.replace(/\//g, "\\") : dir;
}

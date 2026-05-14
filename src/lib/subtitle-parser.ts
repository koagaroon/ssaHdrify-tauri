/**
 * Browser-compatible subtitle parser for timing operations.
 *
 * Replaces the Node.js-only `subsrt` package. Supports:
 * - SRT (SubRip)
 * - VTT (WebVTT)
 * - ASS/SSA (Advanced SubStation Alpha)
 * - SUB (MicroDVD, frame-based)
 *
 * Only implements what we need: parse timestamps, shift them, rebuild.
 * Does NOT attempt full semantic parsing of style tags etc.
 */

export interface Caption {
  /** Raw line(s) from the original file for this caption block */
  raw: string;
  start: number; // ms
  end: number; // ms
  text: string;
  /** VTT cue identifier (lines before the timing line), if present */
  cueId?: string;
  /**
   * ASS-only: placeholder entry for a Dialogue line whose text exceeded
   * MAX_CAPTION_TEXT_LEN. parseAss still emits one Caption per Dialogue
   * line so the parse/build positional contract holds; buildAss returns
   * the original line untouched for entries with `skipped: true`.
   */
  skipped?: boolean;
}

export interface ParseResult {
  format: SubtitleFormat;
  captions: Caption[];
}

export type SubtitleFormat = "srt" | "vtt" | "ass" | "sub" | "unknown";

// ── Format Detection ──────────────────────────────────────

const VTT_HEADER = /^WEBVTT/m;
// SRT hours use variable digit width in practice — many tools emit
// `0:00:01,234` or `1:02:03,456`. This detection regex shape-matches only
// (no parseInt), so an unbounded hour run is harmless here. Numeric
// extraction goes through `parseSrtTime`, which caps hours at `\d{1,12}`
// to keep parseInt from saturating to Infinity on hostile input.
const SRT_TIMING = /\d+:\d{2}:\d{2},\d{3}\s*-->\s*\d+:\d{2}:\d{2},\d{3}/;
const ASS_HEADER = /^\[Script Info\]/im;
const SUB_LINE = /^\{\d+\}\{\d+\}/m;

function normalizeLineEndings(content: string): string {
  return content.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
}

function splitCueBlocks(content: string): string[] {
  return normalizeLineEndings(content)
    .split(/\n[ \t]*\n/)
    .filter((b) => b.trim());
}

export function detectFormat(content: string): SubtitleFormat {
  const head = content.slice(0, 2000); // Check first 2KB
  if (ASS_HEADER.test(head)) return "ass";
  if (VTT_HEADER.test(head)) return "vtt";
  if (SUB_LINE.test(head)) return "sub";
  if (SRT_TIMING.test(head)) return "srt";
  return "unknown";
}

// ── Timestamp Parsing ─────────────────────────────────────

/** Parse "HH:MM:SS,mmm" (SRT) or "HH:MM:SS.mmm" to ms */
function parseSrtTime(ts: string): number {
  // Hours capped at 12 digits — far past any legitimate timestamp,
  // bounded so a 400-digit `\d+` saturating parseInt to Infinity is
  // structurally impossible.
  const m = ts.match(/(\d{1,12}):(\d{2}):(\d{2})[,.](\d{3})/);
  if (!m) return 0;
  return (
    parseInt(m[1], 10) * 3600000 +
    parseInt(m[2], 10) * 60000 +
    parseInt(m[3], 10) * 1000 +
    parseInt(m[4], 10)
  );
}

/** Parse VTT timestamps — supports both "HH:MM:SS.mmm" and "MM:SS.mmm" (no hours) */
function parseVttTime(ts: string): number {
  // HH:MM:SS.mmm (or H:MM:SS.mmm) — WebVTT spec mandates ≥2 hour digits but
  // subsrt and other parsers are lenient. Match any non-empty digit run to
  // stay consistent with parseSrtTime / parseAssTime.
  const full = ts.match(/^(\d{1,12}):(\d{2}):(\d{2})\.(\d{3})$/);
  if (full) {
    return (
      parseInt(full[1], 10) * 3600000 +
      parseInt(full[2], 10) * 60000 +
      parseInt(full[3], 10) * 1000 +
      parseInt(full[4], 10)
    );
  }
  // MM:SS.mmm (no hours — valid per WebVTT spec)
  const short = ts.match(/^(\d{2}):(\d{2})\.(\d{3})$/);
  if (short) {
    return parseInt(short[1], 10) * 60000 + parseInt(short[2], 10) * 1000 + parseInt(short[3], 10);
  }
  return 0;
}

/** Parse "H:MM:SS.cc" (ASS centiseconds) to ms */
function parseAssTime(ts: string): number {
  const m = ts.match(/(\d{1,12}):(\d{2}):(\d{2})\.(\d{2})/);
  if (!m) return 0;
  return (
    parseInt(m[1], 10) * 3600000 +
    parseInt(m[2], 10) * 60000 +
    parseInt(m[3], 10) * 1000 +
    parseInt(m[4], 10) * 10
  );
}

/** Format ms → "HH:MM:SS,mmm" (SRT) */
function formatSrtTime(ms: number): string {
  if (!Number.isFinite(ms)) ms = 0;
  if (ms < 0) ms = 0;
  const h = Math.floor(ms / 3600000);
  const m = Math.floor((ms % 3600000) / 60000);
  const s = Math.floor((ms % 60000) / 1000);
  const mil = ms % 1000;
  return `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")},${String(mil).padStart(3, "0")}`;
}

/** Format ms → "HH:MM:SS.mmm" (VTT) */
function formatVttTime(ms: number): string {
  return formatSrtTime(ms).replace(",", ".");
}

/** Format ms → "H:MM:SS.cc" (ASS centiseconds) */
function formatAssTime(ms: number): string {
  if (!Number.isFinite(ms)) ms = 0;
  if (ms < 0) ms = 0;
  const totalCs = Math.round(ms / 10);
  const cs = totalCs % 100;
  const totalSec = Math.floor(totalCs / 100);
  const h = Math.floor(totalSec / 3600);
  const m = Math.floor((totalSec % 3600) / 60);
  const s = totalSec % 60;
  return `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}.${String(cs).padStart(2, "0")}`;
}

/** Format ms → "HH:MM:SS.mmm" for display */
export function formatDisplayTime(ms: number): string {
  return formatVttTime(ms);
}

/** Parse "HH:MM:SS.mmm" display format back to ms */
export function parseDisplayTime(ts: string): number | null {
  const m = ts.match(/^(\d{1,12}):(\d{2}):(\d{2})\.(\d{1,3})$/);
  if (!m) return null;
  return (
    parseInt(m[1], 10) * 3600000 +
    parseInt(m[2], 10) * 60000 +
    parseInt(m[3], 10) * 1000 +
    parseInt(m[4].padEnd(3, "0"), 10)
  );
}

// ── SRT Parser ────────────────────────────────────────────

// Per-format upper bound on parsed entries. Calibrated to be generous
// for real workflows (a 50 MB SRT runs roughly 350k blocks at ~150B
// each — common for transcription dumps and concatenated archives)
// while still bounding worst-case JS heap from a runaway file.
// Original ASS-side cap was 100k which silently rejected legitimate
// long-form transcripts; 500k matches the per-file face cap on the
// Rust side as a unified "defensive ceiling."
const MAX_PARSED_ENTRIES = 500_000;

// Round 6 Wave 6.8 (Codex Finding 1 fix): raw-block cap restored as
// defense-in-depth alongside the per-caption cap. W6.5 #18 removed
// the raw block cap on the rationale that "ASS/SUB count entries
// not blocks", but `splitCueBlocks` materializes ALL raw blocks
// before the parse loop and the per-caption cap only fires AFTER
// `if (timingIdx === -1) continue` skips junk — so a 50 MB SRT/VTT
// with millions of NOTE-like cue blocks could force the parser to
// scan every one of them without ever incrementing the caption
// counter (~7s + ~774 MB RSS in Codex's PoC; constrained-heap V8
// OOM with `--max-old-space-size=128` and a 24 MB payload). The
// 2M ceiling here is 4× MAX_PARSED_ENTRIES, leaving ample headroom
// for legitimate files with stray blank-line padding while keeping
// pathological junk-flood input bounded. Per-caption cap below
// still owns the semantic limit (and its error message); this is
// a hard ceiling on iteration cost.
const MAX_RAW_BLOCKS = 2_000_000;

function parseSrt(content: string): Caption[] {
  const captions: Caption[] = [];
  // Normalize first so mixed CRLF/LF files still split into cue blocks.
  const blocks = splitCueBlocks(content);
  // W6.8 raw-block ceiling — see MAX_RAW_BLOCKS docblock for the
  // Codex Finding 1 rationale (junk-flood DoS bypass of the
  // per-caption cap). Hard ceiling on iteration cost; the
  // per-caption cap below owns the semantic limit.
  if (blocks.length > MAX_RAW_BLOCKS) {
    throw new Error(
      `Too many subtitle blocks: ${blocks.length} (max ${MAX_RAW_BLOCKS}). ` +
        `Per-caption parse cap is ${MAX_PARSED_ENTRIES}; raw-block ceiling guards iteration cost.`
    );
  }
  // Regex defined inside function — no shared lastIndex state.
  // Hours bounded to 12 digits — accepts the single-digit form some
  // tools emit (`0:00:01,000`), matching detectFormat's SRT_TIMING,
  // while rejecting pathological 100+ digit strings that would saturate
  // parseInt to Infinity.
  const timingRe = /^(\d{1,12}:\d{2}:\d{2},\d{3})\s*-->\s*(\d{1,12}:\d{2}:\d{2},\d{3})/;

  // Round 6 Wave 6.5 #18 / Wave 6.8: per-caption cap below guards the
  // semantic ceiling (`MAX_PARSED_ENTRIES`); the W6.8 raw-block cap
  // above is the iteration-cost guard.
  for (const block of blocks) {
    const lines = block.replace(/^\n/, "").split("\n");
    // Find the timing line (skip the numeric index line)
    let timingIdx = -1;
    for (let i = 0; i < lines.length; i++) {
      if (timingRe.test(lines[i])) {
        timingIdx = i;
        break;
      }
    }
    if (timingIdx === -1) continue;

    const timingMatch = lines[timingIdx].match(timingRe);
    if (!timingMatch) continue;

    if (captions.length >= MAX_PARSED_ENTRIES) {
      throw new Error(`Too many subtitle entries: ${captions.length}+ (max ${MAX_PARSED_ENTRIES})`);
    }

    const text = lines
      .slice(timingIdx + 1)
      .join("\n")
      .trim();
    // Round 10 N-R10-007: per-caption text cap parity with parseAss /
    // parseSub. Pre-R10 a 50 MB SRT with one timing line followed by
    // ~50 MB of caption text produced a single Caption with a 50 MB
    // text payload, which then propagated through the HDR / Shift
    // pipelines (~100 MB UTF-16 in JS memory) and into the downstream
    // buildAss path. parseSrt rebuilds from the captions array (not by
    // walking original content), so no positional-alignment placeholder
    // is needed — drop the oversized entry silently. MAX_RAW_BLOCKS
    // (above) bounds iteration cost.
    if (text.length > MAX_CAPTION_TEXT_LEN) {
      continue;
    }
    captions.push({
      raw: block.trim(),
      start: parseSrtTime(timingMatch[1]),
      end: parseSrtTime(timingMatch[2]),
      text,
    });
  }
  return captions;
}

function buildSrt(captions: Caption[]): string {
  return (
    captions
      .map((c, i) => `${i + 1}\n${formatSrtTime(c.start)} --> ${formatSrtTime(c.end)}\n${c.text}`)
      .join("\n\n") + "\n"
  );
}

// ── VTT Parser ────────────────────────────────────────────

function parseVtt(content: string): Caption[] {
  const captions: Caption[] = [];
  const body = normalizeLineEndings(content).replace(/^WEBVTT[^\n]*\n/, "");
  // Normalize first so mixed CRLF/LF files still split into cue blocks.
  const blocks = splitCueBlocks(body);
  // W6.8 raw-block ceiling — same Codex Finding 1 rationale as parseSrt.
  if (blocks.length > MAX_RAW_BLOCKS) {
    throw new Error(
      `Too many subtitle blocks: ${blocks.length} (max ${MAX_RAW_BLOCKS}). ` +
        `Per-caption parse cap is ${MAX_PARSED_ENTRIES}; raw-block ceiling guards iteration cost.`
    );
  }
  // VTT timing: supports both HH:MM:SS.mmm and MM:SS.mmm.
  // Hour group bounded `\d{2,12}` (Round 8 A-R8-A1-2 / N-R8-N1-1) for
  // parity with parseSrt + parseAss — the unbounded `\d{2,}` form let a
  // crafted single-line input scan O(N) before silent-fallback to zero,
  // and diverged from the rest of the parser family. 12 digits caps the
  // hours at ~1e12, well past any plausible timecode and aligned with
  // ASS DIALOGUE_PATTERN's `\d{1,12}` bound.
  const timingRe =
    /^(\d{2,12}:\d{2}:\d{2}\.\d{3}|\d{2}:\d{2}\.\d{3})\s*-->\s*(\d{2,12}:\d{2}:\d{2}\.\d{3}|\d{2}:\d{2}\.\d{3})/;

  // Per-entry cap (Round 6 Wave 6.5 #18) — see parseSrt for the
  // block-vs-entry rationale.
  for (const block of blocks) {
    const lines = block.replace(/^\n/, "").split("\n");
    // Find the timing line — a cue ID is any line that does NOT contain "-->"
    let timingIdx = -1;
    for (let i = 0; i < lines.length; i++) {
      if (timingRe.test(lines[i])) {
        timingIdx = i;
        break;
      }
    }
    if (timingIdx === -1) continue;

    const timingMatch = lines[timingIdx].match(timingRe);
    if (!timingMatch) continue;

    if (captions.length >= MAX_PARSED_ENTRIES) {
      throw new Error(`Too many subtitle entries: ${captions.length}+ (max ${MAX_PARSED_ENTRIES})`);
    }

    const cueId = timingIdx > 0 ? lines.slice(0, timingIdx).join("\n").trim() : undefined;
    const text = lines
      .slice(timingIdx + 1)
      .join("\n")
      .trim();
    // Round 10 N-R10-007: per-caption text cap parity with parseSrt /
    // parseAss / parseSub. buildVtt rebuilds from the captions array,
    // so no placeholder needed — silently drop the oversized entry.
    if (text.length > MAX_CAPTION_TEXT_LEN) {
      continue;
    }
    captions.push({
      raw: block.trim(),
      start: parseVttTime(timingMatch[1]),
      end: parseVttTime(timingMatch[2]),
      text,
      cueId,
    });
  }
  return captions;
}

function buildVtt(captions: Caption[], header: string = "WEBVTT"): string {
  const lines = [header, ""];
  for (const c of captions) {
    if (c.cueId) {
      lines.push(c.cueId);
    }
    lines.push(`${formatVttTime(c.start)} --> ${formatVttTime(c.end)}`);
    lines.push(c.text);
    lines.push("");
  }
  return lines.join("\n");
}

// ── ASS/SSA Parser (timing only) ─────────────────────────

// Single source of truth for the Dialogue line regex.
// `i`: ASS renderers are case-insensitive on `Dialogue:` — real-world tooling
// sometimes emits `DIALOGUE:`. Leading `[\t ]*` tolerates indentation
// WITHOUT spanning lines: with the `m` flag, `\s*` here would have let the
// engine consume any number of newlines + blank-line whitespace at each line
// anchor before failing to match `Dialogue:` and backtracking — a crafted
// file like `[Script Info]\n` + thousands of empty lines without any
// Dialogue line drove O(n^2) regex work (Codex 3e8a86d0 — ReDoS via
// quadratic backtracking on attacker-controlled subtitle inputs). The
// captured whitespace is preserved via the prefix group so buildAss round-
// trips it exactly. A factory (not a shared instance) is used so each call
// gets a fresh `lastIndex` — guarding against pollution if a previous
// parseAss call threw mid-loop.
// Round 7 Wave 7.5 (A4-R7-4): hour fields bounded to {1,12} digits,
// matching `parseAssTime`. Unbounded `\d+` would let pathological
// `999…9:00:00.00` (100+ hour digits) saturate parseInt to Infinity
// before the time-math even runs. 12 digits is identical to the SRT /
// VTT / display-time bound used elsewhere in this file.
const DIALOGUE_PATTERN = String.raw`^([\t ]*Dialogue:[\t ]*\d+,)(\d{1,12}:\d{2}:\d{2}\.\d{2}),( *)(\d{1,12}:\d{2}:\d{2}\.\d{2}),(.*)$`;
const DIALOGUE_FLAGS = "gim";

function createDialogueRe(): RegExp {
  return new RegExp(DIALOGUE_PATTERN, DIALOGUE_FLAGS);
}

// Round 7 Wave 7.5 (A4-R7-4): per-caption text cap. Real-world ASS
// dialogue lines are < 1 KB even for elaborate styled karaoke. 64 KB
// is generous — guards against a crafted file with a single dialogue
// containing a multi-MB text body (P1b attacker-influenced content).
// Captions exceeding the cap are skipped (text dropped), but for ASS
// parseAss still emits a placeholder Caption with `skipped: true` so
// that buildAss's positional consumption stays aligned with the
// original Dialogue line order (see parseAss / buildAss WHY below).
// Matches parseSub's per-text guard added alongside (parseSub doesn't
// need the placeholder dance because buildSub rebuilds from the
// captions array, not by walking original content).
const MAX_CAPTION_TEXT_LEN = 64_000;

function parseAss(content: string): Caption[] {
  const captions: Caption[] = [];
  const dialogueRe = createDialogueRe();
  let match;
  while ((match = dialogueRe.exec(content)) !== null) {
    if (captions.length >= MAX_PARSED_ENTRIES) {
      // Match the SRT/SUB sibling errors which include the actual
      // count for diagnosis. Strict `>=` here means "we just refused
      // to add the next one"; report N+ to make the boundary clear
      // (we can't get the exact dialogue count without one more
      // iteration, and that's the bound we just refused to cross).
      throw new Error(`Too many subtitle entries: ${captions.length}+ (max ${MAX_PARSED_ENTRIES})`);
    }
    const text = match[5];
    if (text.length > MAX_CAPTION_TEXT_LEN) {
      // Emit a skipped placeholder rather than continuing past the
      // match. buildAss walks the same `dialogueRe` over the original
      // content and consumes captions sequentially; if parseAss
      // silently dropped this entry the index would drift and every
      // subsequent Dialogue line would receive the wrong timestamps.
      // Keeping a placeholder (no text payload retained, so the
      // multi-MB body doesn't linger) lets buildAss recognise the
      // position and return the original line untouched.
      captions.push({
        raw: match[0],
        start: parseAssTime(match[2]),
        end: parseAssTime(match[4]),
        text: "",
        skipped: true,
      });
      continue;
    }
    captions.push({
      raw: match[0],
      start: parseAssTime(match[2]),
      end: parseAssTime(match[4]),
      text,
    });
  }
  return captions;
}

function buildAss(content: string, captions: Caption[]): string {
  // For ASS, we replace timestamps in-place rather than rebuilding.
  const dialogueRe = createDialogueRe();
  let idx = 0;
  const result = content.replace(dialogueRe, (original, prefix, _start, space, _end, rest) => {
    if (idx < captions.length) {
      const c = captions[idx++];
      // Skipped placeholder (oversized text in parseAss): preserve the
      // original Dialogue line verbatim. Advancing idx is what keeps
      // the next non-skipped caption aligned with the next Dialogue
      // match downstream.
      if (c.skipped) return original;
      return `${prefix}${formatAssTime(c.start)},${space}${formatAssTime(c.end)},${rest}`;
    }
    return original;
  });
  // A mismatch means the input changed shape between parseAss and buildAss
  // (or the two sides drifted): the output would carry wrong timestamps.
  // Hard-fail rather than warn; silent timing drift is the worst kind.
  if (idx !== captions.length) {
    // This branch should be unreachable: parseAss + buildAss share
    // the same dialogueRe and walk identical positions. If we get
    // here, it means the regex consumed the input differently
    // between the two passes — typically a sign of stateful regex
    // contamination or a future behavior-changing edit to one but
    // not the other. The error string is intentionally diagnostic-
    // grade rather than user-facing because it's a developer
    // invariant: it surfaces to the user only as the prefix
    // before the colon ("buildAss/parseAss drift:"), which the
    // shift-flow's addLog wraps with msg_timing_error and the
    // file name. If users start reporting it, that's the signal
    // to investigate parser/regex divergence in this file.
    const firstUnconsumed = captions[idx]?.raw ?? "(no raw line captured)";
    const excerpt =
      firstUnconsumed.length > 120 ? `${firstUnconsumed.slice(0, 120)}…` : firstUnconsumed;
    throw new Error(
      `buildAss/parseAss drift: consumed ${idx}/${captions.length} shifted entries; ` +
        `first unconsumed entry index=${idx}, raw="${excerpt}"`
    );
  }
  return result;
}

// ── SUB (MicroDVD) Parser ─────────────────────────────────

const DEFAULT_FPS = 23.976;

function parseSub(content: string, fps: number = DEFAULT_FPS): Caption[] {
  // Reject pathological fps values from MicroDVD `{1}{1}<fps>` lines.
  // Real-world fps is 23.976 / 24 / 25 / 29.97 / 30 / 50 / 60, occasionally
  // up to 120 for variable-frame content. Anything outside [1, 1000] is
  // either parser noise or hostile input — fall back to the default.
  if (!Number.isFinite(fps) || fps < 1 || fps > 1000) fps = DEFAULT_FPS;
  const captions: Caption[] = [];
  // Frame numbers are bounded to 12 digits — 12 ASCII chars fits ~31000
  // years of milliseconds at 60 fps, far past anything legitimate, and
  // rejects pathological `{99...9}` inputs that would otherwise saturate
  // parseInt to Infinity. Matches the time-regex bound below.
  const subLineRe = /^\{(\d{1,12})\}\{(\d{1,12})\}(.*)$/gm;
  let match;
  let count = 0;
  while ((match = subLineRe.exec(content)) !== null) {
    // Round 7 Wave 7.5 (N4-R7-8 / A4-R7-14): per-caption count cap
    // moved BEFORE the push so the throw fires WHEN refusing the
    // entry, matching the SRT/VTT/ASS pattern. Pre-W7.5 the cap was
    // `count += 1; if (count > MAX) throw;` which surfaces "count =
    // MAX + 1" — an off-by-one that misled triage when reading the
    // error message against the documented cap (505000 vs 500000).
    if (count >= MAX_PARSED_ENTRIES) {
      throw new Error(`Too many subtitle entries: ${count}+ (max ${MAX_PARSED_ENTRIES})`);
    }
    // Per-caption text cap (A4-R7-5) — same rationale as parseAss.
    //
    // Round 10 N-R10-006: oversized text now pushes a skipped
    // placeholder that DOES count toward MAX_PARSED_ENTRIES (option (b)
    // — matching parseAss's treatment). Pre-R10 the bare `continue`
    // didn't increment `count`, so iteration was unbounded by the
    // per-caption cap; a crafted 50 MB MicroDVD file with millions of
    // oversized entries could spin the `subLineRe.exec` loop without
    // tripping any ceiling (the 50 MB upstream file cap was the only
    // backstop). buildSub filters skipped placeholders out, so disk
    // output is unchanged.
    const text = match[3];
    if (text.length > MAX_CAPTION_TEXT_LEN) {
      count += 1;
      captions.push({
        raw: match[0],
        start: Math.round((parseInt(match[1], 10) / fps) * 1000),
        end: Math.round((parseInt(match[2], 10) / fps) * 1000),
        text: "",
        skipped: true,
      });
      continue;
    }
    count += 1;
    captions.push({
      raw: match[0],
      start: Math.round((parseInt(match[1], 10) / fps) * 1000),
      end: Math.round((parseInt(match[2], 10) / fps) * 1000),
      text: text.replace(/\|/g, "\n"),
    });
  }
  return captions;
}

function buildSub(captions: Caption[], fps: number = DEFAULT_FPS): string {
  if (!Number.isFinite(fps) || fps <= 0) fps = DEFAULT_FPS;
  return (
    captions
      // Round 10 N-R10-006: parseSub pushes a skipped placeholder for
      // oversized text to bound iteration cost via MAX_PARSED_ENTRIES.
      // Filter those out here so the disk output mirrors the legitimate
      // captions only (placeholders carry empty text and would otherwise
      // emit `{f}{f}` lines with no body, polluting the file).
      .filter((c) => !c.skipped)
      .map((c) => {
        // Round 8 N-R8-N1-3: clamp non-finite / negative timestamps to 0
        // for parity with formatSrtTime / formatAssTime / msToAssTime.
        // After a Time Shift with `--offset` large enough to push captions
        // before t=0, c.start / c.end can land negative; without clamping
        // they produce negative frame counts (`{-23}{15}`) that downstream
        // SUB consumers reject. Same defensive shape the SRT / ASS / VTT
        // builders already use; buildSub was the lone outlier.
        const start = Number.isFinite(c.start) ? Math.max(0, c.start) : 0;
        const end = Number.isFinite(c.end) ? Math.max(0, c.end) : 0;
        const startFrame = Math.round((start / 1000) * fps);
        const endFrame = Math.round((end / 1000) * fps);
        return `{${startFrame}}{${endFrame}}${c.text.replace(/\n/g, "|")}`;
      })
      .join("\n") + "\n"
  );
}

// ── Public API ────────────────────────────────────────────

/**
 * Parse a subtitle file and extract captions with timestamps.
 */
export function parseSubtitle(content: string, fps?: number): ParseResult {
  // Strip a leading BOM. Production callers receive content via the Rust
  // read_text_detect_encoding IPC (which already strips it), so this is
  // defense-in-depth for unit tests and any future internal caller that
  // bypasses the IPC layer with raw fixture content. No subtitle format
  // legitimately starts with U+FEFF, so the strip is safe.
  if (content.charCodeAt(0) === 0xfeff) content = content.slice(1);

  const format = detectFormat(content);
  let captions: Caption[];

  switch (format) {
    case "srt":
      captions = parseSrt(content);
      break;
    case "vtt":
      captions = parseVtt(content);
      break;
    case "ass":
      captions = parseAss(content);
      break;
    case "sub":
      captions = parseSub(content, fps);
      break;
    default:
      throw new Error("Could not detect subtitle format");
  }

  return { format, captions };
}

/**
 * Shift subtitle timestamps and rebuild the file.
 *
 * @param content - Original file content
 * @param offsetMs - Offset in milliseconds (positive = later, negative = earlier)
 * @param thresholdMs - If set, only shift captions starting at or after this time
 * @param fps - Frame rate for SUB format (default 23.976)
 * @returns Shifted subtitle content
 */
export function shiftSubtitle(
  content: string,
  offsetMs: number,
  thresholdMs?: number,
  fps?: number
): { output: string; format: SubtitleFormat; captions: Caption[]; shifted: Caption[] } {
  const { format, captions } = parseSubtitle(content, fps);

  const shifted = captions.map((c) => {
    // Skipped placeholders (ASS-only, oversized Dialogue text) pass
    // through unchanged — buildAss treats them as "preserve original
    // line", so shifting their (synthetic) timestamps would be wasted
    // work and would obscure the placeholder's intent.
    if (c.skipped) return c;
    const shouldShift = thresholdMs === undefined || c.start >= thresholdMs;
    if (!shouldShift) return { ...c };
    return {
      ...c,
      start: Math.max(0, c.start + offsetMs),
      end: Math.max(0, c.end + offsetMs),
    };
  });

  let output: string;
  switch (format) {
    case "srt":
      output = buildSrt(shifted);
      break;
    case "vtt": {
      // Preserve the original VTT header (may contain X-TIMESTAMP-MAP for HLS).
      // Extracted here and passed to buildVtt as a local — no module-level state.
      const headerMatch = content.match(/^(WEBVTT[^\r\n]*)\r?\n/);
      const vttHeader = headerMatch?.[1] ?? "WEBVTT";
      output = buildVtt(shifted, vttHeader);
      break;
    }
    case "ass":
      output = buildAss(content, shifted);
      break;
    case "sub":
      output = buildSub(shifted, fps);
      break;
    default:
      throw new Error(`Cannot rebuild format: ${format}`);
  }

  return { output, format, captions, shifted };
}

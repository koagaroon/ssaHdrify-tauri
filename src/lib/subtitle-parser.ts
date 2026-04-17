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
}

export interface ParseResult {
  format: SubtitleFormat;
  captions: Caption[];
}

export type SubtitleFormat = "srt" | "vtt" | "ass" | "sub" | "unknown";

// ── Format Detection ──────────────────────────────────────

const VTT_HEADER = /^WEBVTT/m;
const SRT_TIMING = /\d{2}:\d{2}:\d{2},\d{3}\s*-->\s*\d{2}:\d{2}:\d{2},\d{3}/;
const ASS_HEADER = /^\[Script Info\]/im;
const SUB_LINE = /^\{\d+\}\{\d+\}/m;

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
  const m = ts.match(/(\d+):(\d{2}):(\d{2})[,.](\d{3})/);
  if (!m) return 0;
  return parseInt(m[1]) * 3600000 + parseInt(m[2]) * 60000 + parseInt(m[3]) * 1000 + parseInt(m[4]);
}

/** Parse VTT timestamps — supports both "HH:MM:SS.mmm" and "MM:SS.mmm" (no hours) */
function parseVttTime(ts: string): number {
  // HH:MM:SS.mmm (or H:MM:SS.mmm with variable-length hours)
  const full = ts.match(/^(\d{2,}):(\d{2}):(\d{2})\.(\d{3})$/);
  if (full) {
    return (
      parseInt(full[1]) * 3600000 +
      parseInt(full[2]) * 60000 +
      parseInt(full[3]) * 1000 +
      parseInt(full[4])
    );
  }
  // MM:SS.mmm (no hours — valid per WebVTT spec)
  const short = ts.match(/^(\d{2}):(\d{2})\.(\d{3})$/);
  if (short) {
    return parseInt(short[1]) * 60000 + parseInt(short[2]) * 1000 + parseInt(short[3]);
  }
  return 0;
}

/** Parse "H:MM:SS.cc" (ASS centiseconds) to ms */
function parseAssTime(ts: string): number {
  const m = ts.match(/(\d+):(\d{2}):(\d{2})\.(\d{2})/);
  if (!m) return 0;
  return (
    parseInt(m[1]) * 3600000 + parseInt(m[2]) * 60000 + parseInt(m[3]) * 1000 + parseInt(m[4]) * 10
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
  const m = ts.match(/^(\d+):(\d{2}):(\d{2})\.(\d{1,3})$/);
  if (!m) return null;
  return (
    parseInt(m[1]) * 3600000 +
    parseInt(m[2]) * 60000 +
    parseInt(m[3]) * 1000 +
    parseInt(m[4].padEnd(3, "0"))
  );
}

// ── SRT Parser ────────────────────────────────────────────

function parseSrt(content: string): Caption[] {
  const captions: Caption[] = [];
  // Split on double-newline (handles both \n\n and \r\n\r\n)
  const blocks = content.split(/\n\n|\r\n\r\n/).filter((b) => b.trim());
  if (blocks.length > 100000) {
    throw new Error(`Too many subtitle blocks: ${blocks.length} (max 100,000)`);
  }
  // Regex defined inside function — no shared lastIndex state
  const timingRe = /^(\d{2}:\d{2}:\d{2},\d{3})\s*-->\s*(\d{2}:\d{2}:\d{2},\d{3})/;

  for (const block of blocks) {
    const lines = block.replace(/^\r?\n/, "").split(/\r?\n/);
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

    const text = lines
      .slice(timingIdx + 1)
      .join("\n")
      .trim();
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
  const body = content.replace(/^WEBVTT[^\r\n]*\r?\n/, "");
  // Split on double-newline (handles both \n\n and \r\n\r\n)
  const blocks = body.split(/\n\n|\r\n\r\n/).filter((b) => b.trim());
  if (blocks.length > 100000) {
    throw new Error(`Too many subtitle blocks: ${blocks.length} (max 100,000)`);
  }
  // VTT timing: supports both HH:MM:SS.mmm and MM:SS.mmm
  const timingRe =
    /^(\d{2,}:\d{2}:\d{2}\.\d{3}|\d{2}:\d{2}\.\d{3})\s*-->\s*(\d{2,}:\d{2}:\d{2}\.\d{3}|\d{2}:\d{2}\.\d{3})/;

  for (const block of blocks) {
    const lines = block.replace(/^\r?\n/, "").split(/\r?\n/);
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

    const cueId = timingIdx > 0 ? lines.slice(0, timingIdx).join("\n").trim() : undefined;
    const text = lines
      .slice(timingIdx + 1)
      .join("\n")
      .trim();
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

function parseAss(content: string): Caption[] {
  const captions: Caption[] = [];
  // Regex defined inside function — no shared lastIndex state
  const dialogueRe =
    /^(Dialogue:\s*\d+,)(\d+:\d{2}:\d{2}\.\d{2}),( *)(\d+:\d{2}:\d{2}\.\d{2}),(.*)$/gm;
  let match;
  while ((match = dialogueRe.exec(content)) !== null) {
    captions.push({
      raw: match[0],
      start: parseAssTime(match[2]),
      end: parseAssTime(match[4]),
      text: match[5],
    });
  }
  return captions;
}

function buildAss(content: string, captions: Caption[]): string {
  // For ASS, we replace timestamps in-place rather than rebuilding
  // Regex defined inside function — no shared lastIndex state
  const dialogueRe =
    /^(Dialogue:\s*\d+,)(\d+:\d{2}:\d{2}\.\d{2}),( *)(\d+:\d{2}:\d{2}\.\d{2}),(.*)$/gm;
  let idx = 0;
  const result = content.replace(dialogueRe, (original, prefix, _start, space, _end, rest) => {
    if (idx < captions.length) {
      const c = captions[idx++];
      return `${prefix}${formatAssTime(c.start)},${space}${formatAssTime(c.end)},${rest}`;
    }
    return original;
  });
  // Verify all shifted entries were consumed — a mismatch means
  // parseAss and buildAss diverged on which lines are Dialogue entries
  if (idx !== captions.length) {
    console.warn(`buildAss: consumed ${idx}/${captions.length} shifted entries`);
  }
  return result;
}

// ── SUB (MicroDVD) Parser ─────────────────────────────────

const DEFAULT_FPS = 23.976;

function parseSub(content: string, fps: number = DEFAULT_FPS): Caption[] {
  if (!Number.isFinite(fps) || fps <= 0) fps = DEFAULT_FPS;
  const captions: Caption[] = [];
  // Regex defined inside function — no shared lastIndex state
  const subLineRe = /^\{(\d+)\}\{(\d+)\}(.*)$/gm;
  let match;
  let count = 0;
  while ((match = subLineRe.exec(content)) !== null) {
    count += 1;
    if (count > 100000) {
      throw new Error(`Too many subtitle entries: ${count} (max 100,000)`);
    }
    captions.push({
      raw: match[0],
      start: Math.round((parseInt(match[1]) / fps) * 1000),
      end: Math.round((parseInt(match[2]) / fps) * 1000),
      text: match[3].replace(/\|/g, "\n"),
    });
  }
  return captions;
}

function buildSub(captions: Caption[], fps: number = DEFAULT_FPS): string {
  if (!Number.isFinite(fps) || fps <= 0) fps = DEFAULT_FPS;
  return (
    captions
      .map((c) => {
        const startFrame = Math.round((c.start / 1000) * fps);
        const endFrame = Math.round((c.end / 1000) * fps);
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

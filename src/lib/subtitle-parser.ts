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
}

export interface ParseResult {
  format: SubtitleFormat;
  captions: Caption[];
}

export type SubtitleFormat = "srt" | "vtt" | "ass" | "sub" | "unknown";

// ── Format Detection ──────────────────────────────────────

const VTT_HEADER = /^WEBVTT/m;
const SRT_TIMING = /\d{2}:\d{2}:\d{2},\d{3}\s*-->\s*\d{2}:\d{2}:\d{2},\d{3}/;
const ASS_HEADER = /^\[Script Info\]/mi;
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

/** Parse "HH:MM:SS,mmm" (SRT) or "HH:MM:SS.mmm" (VTT) to ms */
function parseSrtTime(ts: string): number {
  const m = ts.match(/(\d+):(\d{2}):(\d{2})[,.](\d{3})/);
  if (!m) return 0;
  return (
    parseInt(m[1]) * 3600000 +
    parseInt(m[2]) * 60000 +
    parseInt(m[3]) * 1000 +
    parseInt(m[4])
  );
}

/** Parse "H:MM:SS.cc" (ASS centiseconds) to ms */
function parseAssTime(ts: string): number {
  const m = ts.match(/(\d+):(\d{2}):(\d{2})\.(\d{2})/);
  if (!m) return 0;
  return (
    parseInt(m[1]) * 3600000 +
    parseInt(m[2]) * 60000 +
    parseInt(m[3]) * 1000 +
    parseInt(m[4]) * 10
  );
}

/** Format ms → "HH:MM:SS,mmm" (SRT) */
function formatSrtTime(ms: number): string {
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
  if (ms < 0) ms = 0;
  const h = Math.floor(ms / 3600000);
  const m = Math.floor((ms % 3600000) / 60000);
  const s = Math.floor((ms % 60000) / 1000);
  const cs = Math.floor((ms % 1000) / 10);
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

const SRT_BLOCK_RE =
  /(\d+)\s*\r?\n(\d{2}:\d{2}:\d{2},\d{3})\s*-->\s*(\d{2}:\d{2}:\d{2},\d{3})\s*\r?\n([\s\S]*?)(?=\r?\n\r?\n|\r?\n*$)/g;

function parseSrt(content: string): Caption[] {
  const captions: Caption[] = [];
  let match;
  SRT_BLOCK_RE.lastIndex = 0;
  while ((match = SRT_BLOCK_RE.exec(content)) !== null) {
    captions.push({
      raw: match[0],
      start: parseSrtTime(match[2]),
      end: parseSrtTime(match[3]),
      text: match[4].trim(),
    });
  }
  return captions;
}

function buildSrt(captions: Caption[]): string {
  return captions
    .map(
      (c, i) =>
        `${i + 1}\n${formatSrtTime(c.start)} --> ${formatSrtTime(c.end)}\n${c.text}`
    )
    .join("\n\n") + "\n";
}

// ── VTT Parser ────────────────────────────────────────────

const VTT_BLOCK_RE =
  /(?:^|\n)(?:\d+\s*\n)?(\d{2}:\d{2}:\d{2}\.\d{3})\s*-->\s*(\d{2}:\d{2}:\d{2}\.\d{3})[^\n]*\n([\s\S]*?)(?=\n\n|\n*$)/g;

function parseVtt(content: string): Caption[] {
  const captions: Caption[] = [];
  // Skip WEBVTT header
  const body = content.replace(/^WEBVTT[^\n]*\n/, "");
  let match;
  VTT_BLOCK_RE.lastIndex = 0;
  while ((match = VTT_BLOCK_RE.exec(body)) !== null) {
    captions.push({
      raw: match[0],
      start: parseSrtTime(match[1]), // VTT uses . instead of , but same format
      end: parseSrtTime(match[2]),
      text: match[3].trim(),
    });
  }
  return captions;
}

function buildVtt(captions: Caption[]): string {
  const lines = ["WEBVTT", ""];
  for (const c of captions) {
    lines.push(`${formatVttTime(c.start)} --> ${formatVttTime(c.end)}`);
    lines.push(c.text);
    lines.push("");
  }
  return lines.join("\n");
}

// ── ASS/SSA Parser (timing only) ─────────────────────────

const ASS_DIALOGUE_RE =
  /^(Dialogue:\s*\d+,)(\d+:\d{2}:\d{2}\.\d{2}),(\ *\d+:\d{2}:\d{2}\.\d{2}),(.*)$/gm;

function parseAss(content: string): Caption[] {
  const captions: Caption[] = [];
  let match;
  ASS_DIALOGUE_RE.lastIndex = 0;
  while ((match = ASS_DIALOGUE_RE.exec(content)) !== null) {
    captions.push({
      raw: match[0],
      start: parseAssTime(match[2]),
      end: parseAssTime(match[3].trim()),
      text: match[4],
    });
  }
  return captions;
}

function buildAss(content: string, captions: Caption[]): string {
  // For ASS, we replace timestamps in-place rather than rebuilding
  let idx = 0;
  ASS_DIALOGUE_RE.lastIndex = 0;
  return content.replace(ASS_DIALOGUE_RE, (original, prefix, _start, _end, rest) => {
    if (idx < captions.length) {
      const c = captions[idx++];
      return `${prefix}${formatAssTime(c.start)},${formatAssTime(c.end)},${rest}`;
    }
    return original;
  });
}

// ── SUB (MicroDVD) Parser ─────────────────────────────────

const SUB_LINE_RE = /^\{(\d+)\}\{(\d+)\}(.*)$/gm;
const DEFAULT_FPS = 23.976;

function parseSub(content: string, fps: number = DEFAULT_FPS): Caption[] {
  const captions: Caption[] = [];
  let match;
  SUB_LINE_RE.lastIndex = 0;
  while ((match = SUB_LINE_RE.exec(content)) !== null) {
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
  return captions
    .map((c) => {
      const startFrame = Math.round((c.start / 1000) * fps);
      const endFrame = Math.round((c.end / 1000) * fps);
      return `{${startFrame}}{${endFrame}}${c.text.replace(/\n/g, "|")}`;
    })
    .join("\n") + "\n";
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
    const shouldShift =
      thresholdMs === undefined || c.start >= thresholdMs;
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
    case "vtt":
      output = buildVtt(shifted);
      break;
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

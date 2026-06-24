/**
 * Timing shift engine — browser-compatible subtitle timing adjustment.
 *
 * Uses our custom subtitle-parser (no Node.js dependencies).
 *
 * Supports two modes:
 * 1. Simple: shift all timestamps by a fixed offset
 * 2. Threshold: shift only timestamps after a given time point
 *
 * Formats: ASS, SRT, VTT, SUB (MicroDVD)
 */
import {
  shiftSubtitle,
  transformSubtitleTimings,
  formatDisplayTime,
  parseDisplayTime,
  type Caption,
  type SubtitleFormat,
} from "../../lib/subtitle-parser";
import {
  assertSafeOutputFilename,
  assertSafeOutputPath,
  decomposeInputPath,
} from "../../lib/path-validation";

export type { Caption, SubtitleFormat };
export { formatDisplayTime, parseDisplayTime };

export const MAX_TIMING_OFFSET_MS = 365 * 24 * 3600 * 1000;

export interface ShiftOptions {
  /** Offset in milliseconds (positive = later/slower, negative = earlier/faster) */
  offsetMs: number;
  /** If set, only shift captions starting at or after this timestamp (ms) */
  thresholdMs?: number | undefined;
  /** Frame rate for frame-based formats (SUB/MicroDVD). Defaults to 23.976. */
  fps?: number | undefined;
}

/**
 * Validate the numeric shift params at a CLI engine boundary. Deliberately
 * NOT called inside `shiftSubtitles`: the GUI passes NumberInput-validated
 * values, and the formatter already clamps a stray NaN to 0 — but at the CLI
 * entry that clamp is a MISLEADING success (shiftedCount / preview disagree
 * with the rendered zero-shift). Callers at the trust boundary (the standalone
 * `convertShift` and the chain shift transform) reject up front instead, so
 * the failure is loud rather than a silent no-op.
 */
export function assertFiniteShiftMs(offsetMs: number, thresholdMs?: number | null): void {
  if (!Number.isFinite(offsetMs)) {
    throw new Error(`Invalid offsetMs: expected a finite number, got ${String(offsetMs)}`);
  }
  // thresholdMs is OPTIONAL. The CLI wire form (deno_core op JSON) sends
  // `null` — not `undefined` — when no threshold is set, so both must count
  // as "not provided"; only a present-but-non-finite value is an error.
  if (thresholdMs !== null && thresholdMs !== undefined && !Number.isFinite(thresholdMs)) {
    throw new Error(`Invalid thresholdMs: expected a finite number, got ${String(thresholdMs)}`);
  }
}

export interface PreviewEntry {
  index: number;
  originalStart: number;
  originalEnd: number;
  shiftedStart: number;
  shiftedEnd: number;
  /** Truncated text for DOM efficiency (max ~60 codepoints). Keep this for display. */
  text: string;
  /** Full un-truncated text — used for hover tooltips so long lines remain readable. */
  fullText: string;
  wasShifted: boolean;
}

export interface TimingMapRule {
  /** Rule start in milliseconds. Caption start times at this value match. */
  startMs: number;
  /** Optional exclusive end in milliseconds. Caption start times at this value no longer match. */
  endMs?: number | undefined;
  /** Offset in milliseconds applied to both caption start and end. */
  offsetMs: number;
  /** Disabled rows are ignored. Defaults to true. */
  enabled?: boolean | undefined;
  /** Optional UI/report label for the rule. */
  label?: string | undefined;
}

export interface TimingMapParseResult {
  rules: TimingMapRule[];
}

export interface NormalizedTimingMapRule {
  index: number;
  startMs: number;
  endMs?: number | undefined;
  offsetMs: number;
  label?: string | undefined;
}

export interface CompiledTimingMapSegment {
  startMs: number;
  endMs?: number | undefined;
  rule: NormalizedTimingMapRule;
}

export interface CompiledTimingMap {
  segments: CompiledTimingMapSegment[];
  activeRuleCount: number;
}

export interface TimingMapMatch {
  ruleIndex: number | null;
  appliedOffsetMs: number;
  ruleLabel?: string | undefined;
}

export interface TimingMapPreviewEntry extends PreviewEntry {
  ruleIndex: number | null;
  appliedOffsetMs: number;
  ruleLabel?: string | undefined;
}

export interface ShiftResult {
  /** Shifted subtitle content as string */
  content: string;
  /** Detected format name */
  format: SubtitleFormat;
  /** Preview entries: original and shifted timings */
  preview: PreviewEntry[];
  /** Total number of captions (includes skipped placeholders) */
  captionCount: number;
  /**
   * Count of captions whose text exceeded MAX_CAPTION_TEXT_LEN (64 KB)
   * and were emitted as skipped placeholders by the parser. TimingShift
   * surfaces this via msg_oversized_skipped to close the
   * no-silent-action gap.
   */
  skippedCount: number;
}

export interface TimingMapResult extends Omit<ShiftResult, "preview"> {
  preview: TimingMapPreviewEntry[];
  shiftedCount: number;
  activeRuleCount: number;
}

export interface CompactShiftResult extends Omit<ShiftResult, "preview"> {
  shiftedCount: number;
}

export interface CompactTimingMapResult extends CompactShiftResult {
  activeRuleCount: number;
}

function truncateCodepoints(text: string, max: number): string {
  let cp = 0;
  let out = "";
  for (const ch of text) {
    if (cp >= max) break;
    out += ch;
    cp++;
  }
  return out;
}

function assertFiniteTimingMapMs(value: number, label: string): void {
  if (!Number.isFinite(value)) {
    throw new Error(`Invalid timing map ${label}: expected a finite number, got ${String(value)}`);
  }
  if (!Number.isInteger(value)) {
    throw new Error(`Invalid timing map ${label}: expected an integer millisecond value`);
  }
}

const MAX_TIMING_MAP_RULES = 10_000;
// Match the Rust CLI `--after` parser cap: 100k hours is far beyond
// subtitle reality while keeping timing math in JavaScript safe-integer
// space. Timing maps should not accept looser timestamps than the
// existing simple Time Shift path.
const MAX_TIMING_MAP_TIMESTAMP_MS = 100_000 * 3_600_000 + 59 * 60_000 + 59 * 1000 + 999;

function parseTimingMapTimestampMs(value: unknown, label: string): number {
  if (typeof value === "number") {
    assertTimingMapTimestampMs(value, label);
    return value;
  }
  if (typeof value !== "string") {
    throw new Error(`Invalid timing map ${label}: expected timestamp string or integer ms`);
  }

  const raw = value.trim();
  if (!raw) {
    throw new Error(`Invalid timing map ${label}: expected a timestamp`);
  }
  if (/^\d+$/.test(raw)) {
    const parsed = Number(raw);
    assertTimingMapTimestampMs(parsed, label);
    return parsed;
  }

  const normalized = raw.replace(",", ".");
  const m = normalized.match(/^(\d{1,12}):(\d{2}):(\d{2})(?:\.(\d{1,3}))?$/);
  if (!m) {
    throw new Error(`Invalid timing map ${label}: expected HH:MM:SS.mmm, HH:MM:SS, or integer ms`);
  }
  const minutes = Number(m[2]);
  const seconds = Number(m[3]);
  if (minutes >= 60 || seconds >= 60) {
    throw new Error(`Invalid timing map ${label}: minutes and seconds must be below 60`);
  }
  const parsed =
    Number(m[1]) * 3_600_000 +
    minutes * 60_000 +
    seconds * 1000 +
    Number((m[4] ?? "0").padEnd(3, "0"));
  assertTimingMapTimestampMs(parsed, label);
  return parsed;
}

function assertTimingMapTimestampMs(value: number, label: string): void {
  assertFiniteTimingMapMs(value, label);
  if (value < 0) {
    throw new Error(`Invalid timing map ${label}: timestamp must be >= 0`);
  }
  if (value > MAX_TIMING_MAP_TIMESTAMP_MS) {
    throw new Error(`Invalid timing map ${label}: timestamp exceeds 100000:59:59.999`);
  }
}

function parseUnsignedDurationMs(value: string, label: string): number {
  const factors: Record<string, number> = { h: 3_600_000, m: 60_000, s: 1000, ms: 1 };
  const order: Record<string, number> = { h: 0, m: 1, s: 2, ms: 3 };
  const used = new Set<string>();
  const tokenRe = /(\d+(?:\.\d+)?)(ms|h|m|s)/gy;
  let pos = 0;
  let lastOrder = -1;
  let total = 0;

  while (pos < value.length) {
    tokenRe.lastIndex = pos;
    const m = tokenRe.exec(value);
    if (!m || m.index !== pos) {
      throw new Error(`Invalid timing map ${label}: expected duration like +2.5s or -500ms`);
    }
    const unit = m[2]!;
    if (used.has(unit)) {
      throw new Error(`Invalid timing map ${label}: repeated duration unit '${unit}'`);
    }
    if (order[unit]! <= lastOrder) {
      throw new Error(`Invalid timing map ${label}: duration units must be ordered h, m, s, ms`);
    }
    used.add(unit);
    lastOrder = order[unit]!;
    total += Number(m[1]) * factors[unit]!;
    pos = tokenRe.lastIndex;
  }

  if (used.size === 0 || !Number.isFinite(total) || !Number.isInteger(total)) {
    throw new Error(`Invalid timing map ${label}: expected an integer millisecond duration`);
  }
  return total;
}

function parseTimingMapOffsetMs(value: unknown, label: string): number {
  if (typeof value === "number") {
    assertFiniteTimingMapMs(value, label);
    return value;
  }
  if (typeof value !== "string") {
    throw new Error(`Invalid timing map ${label}: expected signed duration or integer ms`);
  }

  const raw = value.trim();
  if (/^[+-]?\d+$/.test(raw)) {
    const parsed = Number(raw);
    assertFiniteTimingMapMs(parsed, label);
    return parsed;
  }
  const signChar = raw[0];
  if (signChar !== "+" && signChar !== "-") {
    throw new Error(`Invalid timing map ${label}: offset strings must include + or -`);
  }
  const sign = signChar === "-" ? -1 : 1;
  const body = raw.slice(1).trim();
  if (!body) {
    throw new Error(`Invalid timing map ${label}: offset is empty`);
  }
  const magnitude = body.includes(":")
    ? parseTimingMapTimestampMs(body, label)
    : parseUnsignedDurationMs(body, label);
  const parsed = sign * magnitude;
  assertFiniteTimingMapMs(parsed, label);
  return parsed;
}

function parseTimingMapEnabled(value: unknown, label: string): boolean | undefined {
  if (value === undefined || value === null || value === "") return undefined;
  if (typeof value === "boolean") return value;
  if (typeof value === "string") {
    const raw = value.trim().toLowerCase();
    if (!raw) return undefined;
    if (["true", "1", "yes", "on", "enabled"].includes(raw)) return true;
    if (["false", "0", "no", "off", "disabled"].includes(raw)) return false;
  }
  throw new Error(`Invalid timing map ${label}: enabled must be true or false`);
}

function timingMapField(raw: Record<string, unknown>, msName: string, textName: string): unknown {
  return raw[msName] ?? raw[textName];
}

function coerceTimingMapRule(raw: unknown, index: number): TimingMapRule {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    throw new Error(`Invalid timing map rule ${index + 1}: expected object`);
  }
  const record = raw as Record<string, unknown>;
  const enabled = parseTimingMapEnabled(record.enabled, `rule ${index + 1} enabled`);
  const out: TimingMapRule = {
    startMs: parseTimingMapTimestampMs(
      timingMapField(record, "startMs", "start"),
      `rule ${index + 1} start`
    ),
    offsetMs: parseTimingMapOffsetMs(
      timingMapField(record, "offsetMs", "offset"),
      `rule ${index + 1} offset`
    ),
  };
  const end = timingMapField(record, "endMs", "end");
  if (end !== undefined && end !== null && end !== "") {
    out.endMs = parseTimingMapTimestampMs(end, `rule ${index + 1} end`);
  }
  if (enabled !== undefined) {
    out.enabled = enabled;
  }
  if (typeof record.label === "string" && record.label.trim()) {
    out.label = record.label.trim();
  }
  return out;
}

function parseCsvTimingMap(content: string): TimingMapRule[] {
  const rules: TimingMapRule[] = [];
  const lines = content.split(/\r\n|\n|\r/);
  for (let lineIndex = 0; lineIndex < lines.length; lineIndex += 1) {
    const rawLine = lines[lineIndex]!;
    const line = rawLine.trim();
    if (!line || line.startsWith("#")) continue;

    const fields = rawLine.split(",").map((field) => field.trim());
    const lowerFields = fields.map((field) => field.toLowerCase());
    if (lowerFields[0] === "start" && lowerFields.includes("offset")) {
      continue;
    }
    if (fields.length < 3 || fields.length > 5) {
      throw new Error(
        `Invalid timing map line ${lineIndex + 1}: expected start,end,offset[,label[,enabled]]`
      );
    }

    const rawRule: Record<string, unknown> = {
      start: fields[0],
      offset: fields[2],
    };
    if (fields[1]) rawRule.end = fields[1];
    if (fields[3]) rawRule.label = fields[3];
    if (fields[4]) rawRule.enabled = fields[4];
    rules.push(coerceTimingMapRule(rawRule, rules.length));
    if (rules.length > MAX_TIMING_MAP_RULES) {
      throw new Error(`Timing map has too many rules; max ${MAX_TIMING_MAP_RULES}`);
    }
  }
  return rules;
}

export function parseTimingMapText(content: string): TimingMapParseResult {
  const trimmed = content.trim();
  if (!trimmed) {
    throw new Error("Timing map is empty");
  }

  const rules = (() => {
    if (trimmed.startsWith("{") || trimmed.startsWith("[")) {
      const parsed = JSON.parse(trimmed) as unknown;
      const rawRules = Array.isArray(parsed)
        ? parsed
        : parsed &&
            typeof parsed === "object" &&
            Array.isArray((parsed as { rules?: unknown }).rules)
          ? (parsed as { rules: unknown[] }).rules
          : null;
      if (!rawRules) {
        throw new Error("Timing map JSON must be an array or an object with a rules array");
      }
      if (rawRules.length > MAX_TIMING_MAP_RULES) {
        throw new Error(`Timing map has too many rules; max ${MAX_TIMING_MAP_RULES}`);
      }
      return rawRules.map(coerceTimingMapRule);
    }
    return parseCsvTimingMap(content);
  })();

  if (rules.length === 0) {
    throw new Error("Timing map contains no rules");
  }
  normalizeTimingMapRules(rules);
  return { rules };
}

export function normalizeTimingMapRules(rules: TimingMapRule[]): NormalizedTimingMapRule[] {
  if (!Array.isArray(rules)) {
    throw new Error("Timing map rules must be an array");
  }

  const normalized: NormalizedTimingMapRule[] = [];
  for (let i = 0; i < rules.length; i += 1) {
    const rule = rules[i];
    if (!rule) {
      throw new Error(`Invalid timing map rule ${i + 1}: missing rule`);
    }
    if (rule.enabled === false) continue;

    assertFiniteTimingMapMs(rule.startMs, `rule ${i + 1} startMs`);
    assertFiniteTimingMapMs(rule.offsetMs, `rule ${i + 1} offsetMs`);
    if (rule.startMs < 0) {
      throw new Error(`Invalid timing map rule ${i + 1}: startMs must be >= 0`);
    }
    if (rule.startMs > MAX_TIMING_MAP_TIMESTAMP_MS) {
      throw new Error(`Invalid timing map rule ${i + 1}: startMs exceeds 100000:59:59.999`);
    }
    if (Math.abs(rule.offsetMs) > MAX_TIMING_OFFSET_MS) {
      throw new Error(
        `Invalid timing map rule ${i + 1}: offsetMs exceeds +/-${MAX_TIMING_OFFSET_MS} ms`
      );
    }

    const out: NormalizedTimingMapRule = {
      index: i,
      startMs: rule.startMs,
      offsetMs: rule.offsetMs,
    };
    if (rule.endMs !== undefined) {
      assertFiniteTimingMapMs(rule.endMs, `rule ${i + 1} endMs`);
      if (rule.endMs < 0) {
        throw new Error(`Invalid timing map rule ${i + 1}: endMs must be >= 0`);
      }
      if (rule.endMs > MAX_TIMING_MAP_TIMESTAMP_MS) {
        throw new Error(`Invalid timing map rule ${i + 1}: endMs exceeds 100000:59:59.999`);
      }
      if (rule.endMs <= rule.startMs) {
        throw new Error(`Invalid timing map rule ${i + 1}: endMs must be greater than startMs`);
      }
      out.endMs = rule.endMs;
    }
    if (rule.label !== undefined && rule.label.trim()) {
      out.label = rule.label.trim().slice(0, 80);
    }
    normalized.push(out);
  }

  return normalized;
}

function addTimingMapEvent(
  events: Map<number, NormalizedTimingMapRule[]>,
  timeMs: number,
  rule: NormalizedTimingMapRule
): void {
  const current = events.get(timeMs);
  if (current) {
    current.push(rule);
  } else {
    events.set(timeMs, [rule]);
  }
}

function pushRuleHeap(heap: NormalizedTimingMapRule[], rule: NormalizedTimingMapRule): void {
  heap.push(rule);
  let index = heap.length - 1;
  while (index > 0) {
    const parent = Math.floor((index - 1) / 2);
    if (heap[parent]!.index <= rule.index) break;
    heap[index] = heap[parent]!;
    index = parent;
  }
  heap[index] = rule;
}

function popRuleHeap(heap: NormalizedTimingMapRule[]): NormalizedTimingMapRule | undefined {
  if (heap.length === 0) return undefined;
  const top = heap[0]!;
  const last = heap.pop()!;
  if (heap.length === 0) return top;

  let index = 0;
  while (true) {
    const left = index * 2 + 1;
    const right = left + 1;
    if (left >= heap.length) break;

    const child = right < heap.length && heap[right]!.index < heap[left]!.index ? right : left;
    if (heap[child]!.index >= last.index) break;
    heap[index] = heap[child]!;
    index = child;
  }
  heap[index] = last;
  return top;
}

function pushTimingMapSegment(
  segments: CompiledTimingMapSegment[],
  startMs: number,
  endMs: number | undefined,
  rule: NormalizedTimingMapRule
): void {
  if (endMs !== undefined && endMs <= startMs) return;
  const previous = segments[segments.length - 1];
  if (previous && previous.rule.index === rule.index && previous.endMs === startMs) {
    previous.endMs = endMs;
    return;
  }
  segments.push({
    startMs,
    ...(endMs !== undefined && { endMs }),
    rule,
  });
}

export function compileTimingMapRules(rules: NormalizedTimingMapRule[]): CompiledTimingMap {
  const starts = new Map<number, NormalizedTimingMapRule[]>();
  const ends = new Map<number, NormalizedTimingMapRule[]>();
  const boundaries = new Set<number>([0]);

  for (const rule of rules) {
    boundaries.add(rule.startMs);
    addTimingMapEvent(starts, rule.startMs, rule);
    if (rule.endMs !== undefined) {
      boundaries.add(rule.endMs);
      addTimingMapEvent(ends, rule.endMs, rule);
    }
  }

  const sortedBoundaries = Array.from(boundaries).sort((a, b) => a - b);
  const activeRuleIndexes = new Set<number>();
  const priorityHeap: NormalizedTimingMapRule[] = [];
  const segments: CompiledTimingMapSegment[] = [];

  for (let i = 0; i < sortedBoundaries.length; i += 1) {
    const boundary = sortedBoundaries[i]!;
    for (const rule of ends.get(boundary) ?? []) {
      activeRuleIndexes.delete(rule.index);
    }
    for (const rule of starts.get(boundary) ?? []) {
      activeRuleIndexes.add(rule.index);
      pushRuleHeap(priorityHeap, rule);
    }

    while (priorityHeap.length > 0 && !activeRuleIndexes.has(priorityHeap[0]!.index)) {
      popRuleHeap(priorityHeap);
    }

    const winner = priorityHeap[0];
    if (winner) {
      pushTimingMapSegment(segments, boundary, sortedBoundaries[i + 1], winner);
    }
  }

  return { segments, activeRuleCount: rules.length };
}

export function findTimingMapRule(
  captionStartMs: number,
  timingMap: CompiledTimingMap
): NormalizedTimingMapRule | null {
  const { segments } = timingMap;
  let low = 0;
  let high = segments.length - 1;

  while (low <= high) {
    const mid = Math.floor((low + high) / 2);
    const segment = segments[mid]!;
    if (captionStartMs < segment.startMs) {
      high = mid - 1;
    } else if (segment.endMs !== undefined && captionStartMs >= segment.endMs) {
      low = mid + 1;
    } else {
      return segment.rule;
    }
  }
  return null;
}

function countSkippedCaptions(captions: Caption[]): number {
  let count = 0;
  for (const caption of captions) {
    if (caption.skipped) count += 1;
  }
  return count;
}

function countShiftedCaptions(captions: Caption[], shifted: Caption[]): number {
  let count = 0;
  for (let i = 0; i < captions.length; i += 1) {
    const caption = captions[i]!;
    if (caption.skipped) continue;
    const next = shifted[i]!;
    if (caption.start !== next.start || caption.end !== next.end) {
      count += 1;
    }
  }
  return count;
}

function buildPreview(
  captions: Caption[],
  shifted: Caption[],
  matches?: TimingMapMatch[]
): PreviewEntry[] | TimingMapPreviewEntry[] {
  const preview: (PreviewEntry | TimingMapPreviewEntry)[] = [];
  for (let i = 0; i < captions.length; i++) {
    const c = captions[i]!;
    if (c.skipped) continue;
    const s = shifted[i]!;
    const wasShifted = c.start !== s.start || c.end !== s.end;
    const base: PreviewEntry = {
      index: i + 1,
      originalStart: c.start,
      originalEnd: c.end,
      shiftedStart: s.start,
      shiftedEnd: s.end,
      text: truncateCodepoints(c.text, 60),
      fullText: c.text,
      wasShifted,
    };
    const match = matches?.[i];
    if (match) {
      preview.push({
        ...base,
        ruleIndex: match.ruleIndex,
        appliedOffsetMs: match.appliedOffsetMs,
        ...(match.ruleLabel !== undefined && { ruleLabel: match.ruleLabel }),
      });
    } else {
      preview.push(base);
    }
  }
  return preview;
}

/**
 * Parse, shift, and rebuild a subtitle file.
 */
export function shiftSubtitles(content: string, options: ShiftOptions): ShiftResult {
  const { offsetMs, thresholdMs, fps } = options;

  const { output, format, captions, shifted } = shiftSubtitle(content, offsetMs, thresholdMs, fps);
  // Build preview for every caption — the UI scroll container decides
  // how many are visible at a time. `buildPreview` excludes oversized
  // skipped placeholders and preserves original caption indexes.
  const preview = buildPreview(captions, shifted) as PreviewEntry[];

  return {
    content: output,
    format,
    preview,
    captionCount: captions.length,
    skippedCount: captions.filter((c) => c.skipped).length,
  };
}

export function shiftSubtitlesWithTimingMap(
  content: string,
  options: { rules: TimingMapRule[]; fps?: number | undefined }
): TimingMapResult {
  const rules = normalizeTimingMapRules(options.rules);
  const timingMap = compileTimingMapRules(rules);
  const matches: TimingMapMatch[] = [];

  const { output, format, captions, shifted } = transformSubtitleTimings(
    content,
    (caption, index) => {
      const rule = findTimingMapRule(caption.start, timingMap);
      if (!rule) {
        matches[index] = { ruleIndex: null, appliedOffsetMs: 0 };
        return { ...caption };
      }
      matches[index] = {
        ruleIndex: rule.index,
        appliedOffsetMs: rule.offsetMs,
        ...(rule.label !== undefined && { ruleLabel: rule.label }),
      };
      return {
        ...caption,
        start: Math.max(0, caption.start + rule.offsetMs),
        end: Math.max(0, caption.end + rule.offsetMs),
      };
    },
    options.fps
  );

  for (let i = 0; i < captions.length; i += 1) {
    if (captions[i]!.skipped) {
      matches[i] = { ruleIndex: null, appliedOffsetMs: 0 };
    } else if (!matches[i]) {
      matches[i] = { ruleIndex: null, appliedOffsetMs: 0 };
    }
  }

  const preview = buildPreview(captions, shifted, matches) as TimingMapPreviewEntry[];

  return {
    content: output,
    format,
    preview,
    captionCount: captions.length,
    skippedCount: countSkippedCaptions(captions),
    shiftedCount: countShiftedCaptions(captions, shifted),
    activeRuleCount: timingMap.activeRuleCount,
  };
}

export function shiftSubtitlesCompact(content: string, options: ShiftOptions): CompactShiftResult {
  const { offsetMs, thresholdMs, fps } = options;
  const { output, format, captions, shifted } = shiftSubtitle(content, offsetMs, thresholdMs, fps);

  return {
    content: output,
    format,
    captionCount: captions.length,
    skippedCount: countSkippedCaptions(captions),
    shiftedCount: countShiftedCaptions(captions, shifted),
  };
}

export function shiftSubtitlesWithTimingMapCompact(
  content: string,
  options: { rules: TimingMapRule[]; fps?: number | undefined }
): CompactTimingMapResult {
  const rules = normalizeTimingMapRules(options.rules);
  const timingMap = compileTimingMapRules(rules);
  const { output, format, captions, shifted } = transformSubtitleTimings(
    content,
    (caption) => {
      const rule = findTimingMapRule(caption.start, timingMap);
      if (!rule) return { ...caption };
      return {
        ...caption,
        start: Math.max(0, caption.start + rule.offsetMs),
        end: Math.max(0, caption.end + rule.offsetMs),
      };
    },
    options.fps
  );

  return {
    content: output,
    format,
    captionCount: captions.length,
    skippedCount: countSkippedCaptions(captions),
    shiftedCount: countShiftedCaptions(captions, shifted),
    activeRuleCount: timingMap.activeRuleCount,
  };
}

/**
 * Derive the `.shifted` output path for a given input subtitle path.
 *
 * Used by the batch save flow — Time Shift writes outputs alongside
 * inputs with a `.shifted` infix, preserving the original extension
 * (`EP01.srt` → `EP01.shifted.srt`, `EP01.ass` → `EP01.shifted.ass`).
 * The native separator of the input path is preserved so the result
 * round-trips through Win32 APIs and shell-integration tools without
 * mixing slashes.
 *
 * Why a derived path instead of a per-file native save dialog: those
 * dialogs are blocking and don't scale to N files. The same-directory
 * convention matches the most common workflow (shift in place beside
 * the existing subs) and gives the user a single overwrite-confirm
 * gate via `countExistingFiles` before the batch begins.
 */
export function deriveShiftedPath(inputPath: string): string {
  // Decompose via the shared helper. Validates absolute, accepts drive-
  // root files (`C:\foo.ass`), rejects drive-relative (`C:foo.ass`).
  const parts = decomposeInputPath(inputPath);
  const { dir, ext, normalized, usedBackslash } = parts;
  let { baseName } = parts;
  // Strip any prior `.shifted` infix so re-shifting `EP01.shifted.ass`
  // yields `EP01.shifted.ass` (idempotent) rather than the cumulative
  // `EP01.shifted.shifted.ass`. Mirrors the strip-and-re-apply pattern
  // resolveOutputPath uses for the HDR `.hdr` infix.
  if (baseName.toLowerCase().endsWith(".shifted")) {
    baseName = baseName.slice(0, -".shifted".length);
  }
  // after the `.shifted` strip the baseName can
  // be empty (or whitespace-/dot-only — POSIX dotfile shapes like
  // `.shifted.srt` whose stem is just `.`). Without this re-check
  // the output name resolves to `.shifted${ext}` — identical to the
  // input on subsequent invocations and visually weird as a filename.
  // `cli-engine-entry.ts::resolveShiftOutputPathInternal` has the same
  // guard; mirror it here for the GUI path so error messages stay
  // aligned across cheap (this function) and heavy (CLI engine)
  // paths.
  if (!baseName || !baseName.replace(/^\.+/, "").trim()) {
    throw new Error("Input filename has no valid stem after stripping .shifted infix");
  }
  const outputName = `${baseName}.shifted${ext}`;
  // Apply the shared safety checks (reserved names, traversal,
  // MAX_PATH, self-overwrite). Same helpers as HDR / Embed resolvers.
  assertSafeOutputFilename(outputName);
  const outputPath = `${dir}/${outputName}`;
  assertSafeOutputPath(outputPath, normalized);
  return usedBackslash ? outputPath.replace(/\//g, "\\") : outputPath;
}

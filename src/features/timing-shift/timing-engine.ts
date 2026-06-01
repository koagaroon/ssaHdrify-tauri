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

function parseTimingMapTimestampMs(value: unknown, label: string): number {
  if (typeof value === "number") {
    assertFiniteTimingMapMs(value, label);
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
    assertFiniteTimingMapMs(parsed, label);
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
  assertFiniteTimingMapMs(parsed, label);
  return parsed;
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

export function findTimingMapRule(
  captionStartMs: number,
  rules: NormalizedTimingMapRule[]
): NormalizedTimingMapRule | null {
  // First enabled matching rule wins. This keeps overlapping maps
  // deterministic and gives future GUI rule order real meaning.
  for (const rule of rules) {
    if (captionStartMs < rule.startMs) continue;
    if (rule.endMs !== undefined && captionStartMs >= rule.endMs) continue;
    return rule;
  }
  return null;
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
  const matches: TimingMapMatch[] = [];

  const { output, format, captions, shifted } = transformSubtitleTimings(
    content,
    (caption, index) => {
      const rule = findTimingMapRule(caption.start, rules);
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
    skippedCount: captions.filter((c) => c.skipped).length,
    shiftedCount: preview.filter((entry) => entry.wasShifted).length,
    activeRuleCount: rules.length,
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

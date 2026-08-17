import { describe, expect, it } from "vitest";

import {
  MAX_TIMING_PREVIEW_ENTRIES,
  MAX_TIMING_PREVIEW_TOOLTIP_CODEPOINTS,
  MAX_TIMING_OFFSET_MS,
  compileTimingMapRules,
  findTimingMapRule,
  normalizeTimingMapRules,
  parseTimingMapText,
  previewShiftSubtitles,
  shiftSubtitles,
  shiftSubtitlesCompact,
  shiftSubtitlesWithTimingMap,
  shiftSubtitlesWithTimingMapCompact,
} from "./timing-engine";

const SRT_SAMPLE = [
  "1",
  "00:00:01,000 --> 00:00:02,000",
  "one",
  "",
  "2",
  "00:00:05,000 --> 00:00:06,000",
  "two",
  "",
  "3",
  "00:00:09,000 --> 00:00:10,000",
  "three",
  "",
].join("\n");

describe("shiftSubtitles", () => {
  it("keeps the existing threshold-gated global shift behavior", () => {
    const result = shiftSubtitles(SRT_SAMPLE, { offsetMs: 1000, thresholdMs: 5000 });

    expect(result.preview.map((entry) => entry.wasShifted)).toEqual([false, true, true]);
    expect(result.content).toContain("00:00:01,000 --> 00:00:02,000");
    expect(result.content).toContain("00:00:06,000 --> 00:00:07,000");
    expect(result.content).toContain("00:00:10,000 --> 00:00:11,000");
  });

  it("uses a parse-only preview that matches full shift timings without returning content", () => {
    const options = { offsetMs: 1000.9, thresholdMs: 5000 };
    const preview = previewShiftSubtitles(SRT_SAMPLE, options);
    const full = shiftSubtitles(SRT_SAMPLE, options);

    expect("content" in preview).toBe(false);
    expect(preview.format).toBe(full.format);
    expect(preview.captionCount).toBe(full.captionCount);
    expect(preview.shiftableCount).toBe(full.captionCount - full.skippedCount);
    expect(preview.skippedCount).toBe(full.skippedCount);
    expect(preview.maxCaptionStart).toBe(9000);
    expect(preview.preview).toEqual(full.preview);
    expect(preview.previewTruncated).toBe(false);
  });

  it("caps retained preview rows at the exact boundary while preserving full-file metadata", () => {
    const makeSrt = (count: number) =>
      Array.from({ length: count }, (_, index) => {
        const hours = String(Math.floor(index / 3600)).padStart(2, "0");
        const minutes = String(Math.floor((index % 3600) / 60)).padStart(2, "0");
        const seconds = String(index % 60).padStart(2, "0");
        return `${index + 1}\n${hours}:${minutes}:${seconds},000 --> ${hours}:${minutes}:${seconds},500\ncaption ${index + 1}`;
      }).join("\n\n");

    const atBoundary = previewShiftSubtitles(makeSrt(MAX_TIMING_PREVIEW_ENTRIES), {
      offsetMs: 1,
    });
    const overBoundary = previewShiftSubtitles(makeSrt(MAX_TIMING_PREVIEW_ENTRIES + 1), {
      offsetMs: 1,
    });

    expect(MAX_TIMING_PREVIEW_ENTRIES).toBe(5_000);
    expect(atBoundary.preview).toHaveLength(MAX_TIMING_PREVIEW_ENTRIES);
    expect(atBoundary.previewTruncated).toBe(false);
    expect(overBoundary.captionCount).toBe(MAX_TIMING_PREVIEW_ENTRIES + 1);
    expect(overBoundary.shiftableCount).toBe(MAX_TIMING_PREVIEW_ENTRIES + 1);
    expect(overBoundary.preview).toHaveLength(MAX_TIMING_PREVIEW_ENTRIES);
    expect(overBoundary.preview.at(-1)?.index).toBe(MAX_TIMING_PREVIEW_ENTRIES);
    expect(overBoundary.previewTruncated).toBe(true);
    // Metadata scans the full parsed file rather than only retained rows.
    expect(overBoundary.maxCaptionStart).toBe(MAX_TIMING_PREVIEW_ENTRIES * 1000);
  });

  it("counts oversized placeholders separately from shiftable preview rows", () => {
    const input =
      `1\n00:00:01,000 --> 00:00:02,000\n${"Z".repeat(65_000)}\n\n` +
      "2\n00:00:03,000 --> 00:00:04,000\nNORMAL\n";

    const result = previewShiftSubtitles(input, { offsetMs: 1000 });

    expect(result.captionCount).toBe(2);
    expect(result.shiftableCount).toBe(1);
    expect(result.skippedCount).toBe(1);
    expect(result.preview).toHaveLength(1);
    expect(result.previewTruncated).toBe(false);
  });

  it("caps tooltip text by Unicode code point without splitting surrogate pairs", () => {
    const longCaption = "🙂".repeat(MAX_TIMING_PREVIEW_TOOLTIP_CODEPOINTS + 5);
    const input = `1\n00:00:01,000 --> 00:00:02,000\n${longCaption}\n`;

    const result = previewShiftSubtitles(input, { offsetMs: 0 });
    const tooltip = result.preview[0]!.tooltipText;

    expect(MAX_TIMING_PREVIEW_TOOLTIP_CODEPOINTS).toBe(512);
    expect(Array.from(tooltip)).toHaveLength(MAX_TIMING_PREVIEW_TOOLTIP_CODEPOINTS);
    expect(tooltip.endsWith("…")).toBe(true);
    expect(tooltip.includes("\uFFFD")).toBe(false);
  });
});

describe("shiftSubtitlesWithTimingMap", () => {
  it("applies multiple non-overlapping rules and records preview rule matches", () => {
    const result = shiftSubtitlesWithTimingMap(SRT_SAMPLE, {
      rules: [
        { startMs: 0, endMs: 5000, offsetMs: 1000, label: "opening" },
        { startMs: 5000, offsetMs: -500, label: "main" },
      ],
    });

    expect(result.activeRuleCount).toBe(2);
    expect(result.shiftedCount).toBe(3);
    expect(result.preview.map((entry) => entry.ruleIndex)).toEqual([0, 1, 1]);
    expect(result.preview.map((entry) => entry.appliedOffsetMs)).toEqual([1000, -500, -500]);
    expect(result.preview.map((entry) => entry.ruleLabel)).toEqual(["opening", "main", "main"]);
    expect(result.content).toContain("00:00:02,000 --> 00:00:03,000");
    expect(result.content).toContain("00:00:04,500 --> 00:00:05,500");
    expect(result.content).toContain("00:00:08,500 --> 00:00:09,500");
  });

  it("uses start-inclusive and end-exclusive boundaries", () => {
    const result = shiftSubtitlesWithTimingMap(SRT_SAMPLE, {
      rules: [
        { startMs: 0, endMs: 5000, offsetMs: 1000 },
        { startMs: 5000, endMs: 9000, offsetMs: 2000 },
        { startMs: 9000, offsetMs: 3000 },
      ],
    });

    expect(result.preview.map((entry) => entry.ruleIndex)).toEqual([0, 1, 2]);
    expect(result.content).toContain("00:00:02,000 --> 00:00:03,000");
    expect(result.content).toContain("00:00:07,000 --> 00:00:08,000");
    expect(result.content).toContain("00:00:12,000 --> 00:00:13,000");
  });

  it("uses first-match-wins for overlapping enabled rules", () => {
    const result = shiftSubtitlesWithTimingMap(SRT_SAMPLE, {
      rules: [
        { startMs: 0, endMs: 10_000, offsetMs: 1000 },
        { startMs: 5000, endMs: 6000, offsetMs: 5000 },
      ],
    });

    expect(result.preview.map((entry) => entry.ruleIndex)).toEqual([0, 0, 0]);
    expect(result.preview[1]!.shiftedStart).toBe(6000);
    expect(result.content).toContain("00:00:06,000 --> 00:00:07,000");
  });

  it("ignores disabled rules without changing their original rule indexes", () => {
    const result = shiftSubtitlesWithTimingMap(SRT_SAMPLE, {
      rules: [
        { startMs: 0, offsetMs: 9000, enabled: false },
        { startMs: 0, offsetMs: 1000 },
      ],
    });

    expect(result.activeRuleCount).toBe(1);
    expect(result.preview.map((entry) => entry.ruleIndex)).toEqual([1, 1, 1]);
    expect(result.content).toContain("00:00:02,000 --> 00:00:03,000");
  });

  it("leaves captions unmatched when no enabled rule covers their start time", () => {
    const result = shiftSubtitlesWithTimingMap(SRT_SAMPLE, {
      rules: [{ startMs: 5000, endMs: 9000, offsetMs: 1000 }],
    });

    expect(result.shiftedCount).toBe(1);
    expect(result.preview.map((entry) => entry.ruleIndex)).toEqual([null, 0, null]);
    expect(result.preview.map((entry) => entry.appliedOffsetMs)).toEqual([0, 1000, 0]);
    expect(result.content).toContain("00:00:01,000 --> 00:00:02,000");
    expect(result.content).toContain("00:00:06,000 --> 00:00:07,000");
    expect(result.content).toContain("00:00:09,000 --> 00:00:10,000");
  });

  it("preserves first-match priority through compiled timing-map lookup", () => {
    const rules = normalizeTimingMapRules([
      { startMs: 1000, endMs: 6000, offsetMs: 1000 },
      { startMs: 2000, endMs: 3000, offsetMs: 9000 },
      { startMs: 6000, offsetMs: -500 },
    ]);
    const compiled = compileTimingMapRules(rules);

    expect(findTimingMapRule(500, compiled)).toBeNull();
    expect(findTimingMapRule(2000, compiled)?.index).toBe(0);
    expect(findTimingMapRule(5999, compiled)?.index).toBe(0);
    expect(findTimingMapRule(6000, compiled)?.index).toBe(2);
  });

  it("returns compact results for CLI-style shift paths without preview payloads", () => {
    const simple = shiftSubtitlesCompact(SRT_SAMPLE, { offsetMs: 1000, thresholdMs: 5000 });
    const mapped = shiftSubtitlesWithTimingMapCompact(SRT_SAMPLE, {
      rules: [{ startMs: 5000, endMs: 9000, offsetMs: 1000 }],
    });

    expect("preview" in simple).toBe(false);
    expect(simple.shiftedCount).toBe(2);
    expect("preview" in mapped).toBe(false);
    expect(mapped.shiftedCount).toBe(1);
    expect(mapped.activeRuleCount).toBe(1);
  });
});

describe("parseTimingMapText", () => {
  it("parses app-owned JSON timing maps with timestamp strings", () => {
    const parsed = parseTimingMapText(
      JSON.stringify({
        rules: [
          {
            start: "00:00:00.000",
            end: "00:00:05.000",
            offset: "+1.25s",
            label: "opening",
          },
          { startMs: 5000, offsetMs: -500, enabled: true },
        ],
      })
    );

    expect(parsed.rules).toEqual([
      { startMs: 0, endMs: 5000, offsetMs: 1250, label: "opening" },
      { startMs: 5000, offsetMs: -500, enabled: true },
    ]);
  });

  it("parses CSV timing maps and applies them through the shared engine", () => {
    const parsed = parseTimingMapText(
      [
        "# start,end,offset,label,enabled",
        "start,end,offset,label,enabled",
        "00:00:00.000,00:00:05.000,+1s,opening,true",
        "00:00:05.000,,-500ms,main,true",
      ].join("\n")
    );

    const result = shiftSubtitlesWithTimingMap(SRT_SAMPLE, parsed);

    expect(parsed.rules).toHaveLength(2);
    expect(result.preview.map((entry) => entry.ruleLabel)).toEqual(["opening", "main", "main"]);
    expect(result.content).toContain("00:00:02,000 --> 00:00:03,000");
    expect(result.content).toContain("00:00:04,500 --> 00:00:05,500");
  });

  it("accepts timing-map timestamps at the shared 100000-hour cap", () => {
    const parsed = parseTimingMapText("100000:59:59.999,,+1s");

    expect(parsed.rules).toEqual([{ startMs: 360_003_599_999, offsetMs: 1000 }]);
  });

  it("rejects timing-map timestamps beyond the shared 100000-hour cap", () => {
    expect(() => parseTimingMapText("100001:00:00.000,,+1s")).toThrow(/exceeds/);
    expect(() =>
      parseTimingMapText(JSON.stringify([{ startMs: 360_003_600_000, offsetMs: 0 }]))
    ).toThrow(/exceeds/);
  });

  it("rejects malformed timing-map imports before conversion", () => {
    expect(() => parseTimingMapText("")).toThrow(/empty/);
    expect(() => parseTimingMapText("[{}]")).toThrow(/start/);
    expect(() => parseTimingMapText("00:60:00.000,,+1s")).toThrow(/below 60/);
    expect(() => parseTimingMapText("00:00:00.000,,1s")).toThrow(/include \+ or -/);
  });
});

describe("normalizeTimingMapRules", () => {
  it("rejects invalid timing-map rows before output generation", () => {
    expect(() => normalizeTimingMapRules([{ startMs: -1, offsetMs: 0 }])).toThrow(/startMs/);
    expect(() => normalizeTimingMapRules([{ startMs: 1000, endMs: 1000, offsetMs: 0 }])).toThrow(
      /endMs/
    );
    expect(() => normalizeTimingMapRules([{ startMs: 1000, endMs: 999, offsetMs: 0 }])).toThrow(
      /endMs/
    );
    expect(() => normalizeTimingMapRules([{ startMs: 0, offsetMs: NaN }])).toThrow(/offsetMs/);
    expect(() =>
      normalizeTimingMapRules([{ startMs: 0, offsetMs: MAX_TIMING_OFFSET_MS + 1 }])
    ).toThrow(/exceeds/);
  });

  it("accepts the exact offset cap boundary", () => {
    expect(normalizeTimingMapRules([{ startMs: 0, offsetMs: MAX_TIMING_OFFSET_MS }])).toHaveLength(
      1
    );
    expect(normalizeTimingMapRules([{ startMs: 0, offsetMs: -MAX_TIMING_OFFSET_MS }])).toHaveLength(
      1
    );
  });
});

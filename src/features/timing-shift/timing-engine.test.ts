import { describe, expect, it } from "vitest";

import {
  MAX_TIMING_OFFSET_MS,
  normalizeTimingMapRules,
  parseTimingMapText,
  shiftSubtitles,
  shiftSubtitlesWithTimingMap,
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

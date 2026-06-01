import { describe, expect, it } from "vitest";

import {
  MAX_TIMING_OFFSET_MS,
  normalizeTimingMapRules,
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

import { describe, expect, it } from "vitest";

import {
  formatSignedOffsetMs,
  formatTimingOffsetMagnitude,
  isTimingOffsetInvalid,
  isTimingSaveDisabled,
  parseTimingOffsetMagnitudeMs,
} from "./timing-ui-state";

describe("timing UI state guards", () => {
  it("requires a nonnegative integer literal in millisecond mode", () => {
    for (const text of ["", "not a number", "12abc", "1e2", "1.0", "0.5", "-1", "+1"]) {
      expect(isTimingOffsetInvalid(text, "ms", 1000), text).toBe(true);
    }
    expect(parseTimingOffsetMagnitudeMs("0", "ms", 1000)).toBe(0);
    expect(parseTimingOffsetMagnitudeMs("1000", "ms", 1000)).toBe(1000);
    expect(isTimingOffsetInvalid("1001", "ms", 1000)).toBe(true);
  });

  it("accepts seconds only when they resolve exactly to integer milliseconds", () => {
    expect(parseTimingOffsetMagnitudeMs("2.5", "s", 10)).toBe(2500);
    expect(parseTimingOffsetMagnitudeMs(".001", "s", 10)).toBe(1);
    expect(parseTimingOffsetMagnitudeMs("10.000", "s", 10)).toBe(10000);
    for (const text of ["-0.5", "0.0001", "1.2345", "1e2", "10.001"]) {
      expect(isTimingOffsetInvalid(text, "s", 10), text).toBe(true);
    }
  });

  it("formats unit switches without changing the effective magnitude", () => {
    expect(formatTimingOffsetMagnitude(2500, "s")).toBe("2.5");
    expect(formatTimingOffsetMagnitude(2500, "ms")).toBe("2500");
    expect(formatTimingOffsetMagnitude(1, "s")).toBe("0.001");
    expect(formatTimingOffsetMagnitude(0, "s")).toBe("0");
  });

  it("disables Save when the visible offset is invalid", () => {
    expect(
      isTimingSaveDisabled({
        fileCount: 1,
        thresholdInvalid: false,
        offsetInvalid: true,
        busy: false,
      })
    ).toBe(true);
  });

  it("enables Save only when all prerequisites are satisfied", () => {
    expect(
      isTimingSaveDisabled({
        fileCount: 1,
        thresholdInvalid: false,
        offsetInvalid: false,
        busy: false,
      })
    ).toBe(false);
  });

  it("gates Save on timingMapReady rather than offset inputs in map mode", () => {
    expect(
      isTimingSaveDisabled({
        fileCount: 1,
        thresholdInvalid: true,
        offsetInvalid: true,
        busy: false,
        mode: "map",
        timingMapReady: false,
      })
    ).toBe(true);
    expect(
      isTimingSaveDisabled({
        fileCount: 1,
        thresholdInvalid: true,
        offsetInvalid: true,
        busy: false,
        mode: "map",
        timingMapReady: true,
      })
    ).toBe(false);
    expect(formatSignedOffsetMs(1500)).toBe("+1500 ms");
    expect(formatSignedOffsetMs(-500)).toBe("-500 ms");
    expect(formatSignedOffsetMs(0)).toBe("0 ms");
  });
});

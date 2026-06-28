import { describe, expect, it } from "vitest";

import { isTimingOffsetInvalid, isTimingSaveDisabled } from "./timing-ui-state";

describe("timing UI state guards", () => {
  it("marks blank, non-finite, and out-of-range offsets invalid", () => {
    expect(isTimingOffsetInvalid("", 1000)).toBe(true);
    expect(isTimingOffsetInvalid("not a number", 1000)).toBe(true);
    expect(isTimingOffsetInvalid("12abc", 1000)).toBe(true);
    expect(isTimingOffsetInvalid("1e", 1000)).toBe(true);
    expect(isTimingOffsetInvalid("1e+", 1000)).toBe(true);
    expect(isTimingOffsetInvalid("1e-", 1000)).toBe(true);
    expect(isTimingOffsetInvalid("1001", 1000)).toBe(true);
    expect(isTimingOffsetInvalid("-1001", 1000)).toBe(true);
  });

  it("accepts finite offsets at the exact visible boundary", () => {
    expect(isTimingOffsetInvalid("1000", 1000)).toBe(false);
    expect(isTimingOffsetInvalid("-1000", 1000)).toBe(false);
    expect(isTimingOffsetInvalid("2.5", 10)).toBe(false);
    expect(isTimingOffsetInvalid("1e2", 1000)).toBe(false);
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
});

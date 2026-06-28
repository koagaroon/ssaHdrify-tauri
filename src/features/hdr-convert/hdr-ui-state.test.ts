import { describe, expect, it } from "vitest";

import { isHdrBrightnessInvalid, isHdrConvertDisabled } from "./hdr-ui-state";

describe("HDR UI state guards", () => {
  it("marks blank, non-finite, and out-of-range brightness invalid", () => {
    expect(isHdrBrightnessInvalid("", 1, 10_000)).toBe(true);
    expect(isHdrBrightnessInvalid("not a number", 1, 10_000)).toBe(true);
    expect(isHdrBrightnessInvalid("0", 1, 10_000)).toBe(true);
    expect(isHdrBrightnessInvalid("10001", 1, 10_000)).toBe(true);
  });

  it("accepts finite brightness at the exact visible boundary", () => {
    expect(isHdrBrightnessInvalid("1", 1, 10_000)).toBe(false);
    expect(isHdrBrightnessInvalid("10000", 1, 10_000)).toBe(false);
    expect(isHdrBrightnessInvalid("100.5", 1, 10_000)).toBe(false);
  });

  it("disables Convert when visible brightness is invalid", () => {
    expect(
      isHdrConvertDisabled({
        hasFiles: true,
        processing: false,
        brightnessInvalid: true,
      })
    ).toBe(true);
  });

  it("enables Convert only when all prerequisites are satisfied", () => {
    expect(
      isHdrConvertDisabled({
        hasFiles: true,
        processing: false,
        brightnessInvalid: false,
      })
    ).toBe(false);
  });
});

import { describe, expect, it } from "vitest";

import {
  hasMicroDvdInput,
  isHdrFpsInvalid,
  parseHdrFpsOverride,
  parseHdrFpsOverrideForInput,
  parseHdrStyleNumberInput,
} from "./hdr-style-ui-state";

describe("HDR style UI state", () => {
  it("rejects malformed numeric-prefix style values", () => {
    expect(parseHdrStyleNumberInput("12abc", 0, 200)).toBeNull();
    expect(parseHdrStyleNumberInput("1e", 0, 200)).toBeNull();
    expect(parseHdrStyleNumberInput("1e+", 0, 200)).toBeNull();
    expect(parseHdrStyleNumberInput("1e-", 0, 200)).toBeNull();
    expect(parseHdrStyleNumberInput("Infinity", 0, 200)).toBeNull();
    expect(parseHdrStyleNumberInput("1e309", 0, 200)).toBeNull();
  });

  it("rejects out-of-range style values", () => {
    expect(parseHdrStyleNumberInput("0", 1, 200)).toBeNull();
    expect(parseHdrStyleNumberInput("201", 1, 200)).toBeNull();
  });

  it("accepts complete finite in-range style values", () => {
    expect(parseHdrStyleNumberInput("48", 1, 200)).toBe(48);
    expect(parseHdrStyleNumberInput("1e2", 1, 200)).toBe(100);
    expect(parseHdrStyleNumberInput("0.5", 0, 20)).toBe(0.5);
  });

  it("represents Auto FPS as no explicit override", () => {
    expect(parseHdrFpsOverride("auto", "not used")).toBeUndefined();
    expect(isHdrFpsInvalid("auto", "")).toBe(false);
  });

  it("accepts only a finite manual FPS greater than 3 and at most 120", () => {
    expect(parseHdrFpsOverride("manual", "23.976")).toBe(23.976);
    expect(parseHdrFpsOverride("manual", "120")).toBe(120);
    for (const invalid of ["", "3", "120.001", "NaN", "Infinity", "25fps", "1e"]) {
      expect(parseHdrFpsOverride("manual", invalid)).toBeNull();
      expect(isHdrFpsInvalid("manual", invalid)).toBe(true);
    }
  });

  it("applies manual FPS validation only to MicroDVD inputs", () => {
    expect(hasMicroDvdInput(["episode.srt", "episode.ass"])).toBe(false);
    expect(hasMicroDvdInput(["episode.srt", "EPISODE.SUB"])).toBe(true);
    expect(parseHdrFpsOverrideForInput("episode.srt", "manual", "bad")).toBeUndefined();
    expect(parseHdrFpsOverrideForInput("episode.sub", "manual", "bad")).toBeNull();
    expect(parseHdrFpsOverrideForInput("episode.sub", "manual", "25")).toBe(25);
  });
});

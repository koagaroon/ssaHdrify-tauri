import { describe, expect, it } from "vitest";

import { parseHdrStyleNumberInput } from "./hdr-style-ui-state";

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
});

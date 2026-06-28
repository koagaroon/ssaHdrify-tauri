import { describe, expect, it } from "vitest";

import { parseFiniteNumberText } from "./strict-number";

describe("parseFiniteNumberText", () => {
  it("rejects blank, partial, prefix-only, and non-finite number text", () => {
    expect(parseFiniteNumberText("")).toBeNull();
    expect(parseFiniteNumberText("   ")).toBeNull();
    expect(parseFiniteNumberText("12abc")).toBeNull();
    expect(parseFiniteNumberText("1e")).toBeNull();
    expect(parseFiniteNumberText("1e+")).toBeNull();
    expect(parseFiniteNumberText("1e-")).toBeNull();
    expect(parseFiniteNumberText("Infinity")).toBeNull();
    expect(parseFiniteNumberText("0x10")).toBeNull();
  });

  it("rejects overlong numeric-looking text before conversion", () => {
    expect(parseFiniteNumberText("1".repeat(129))).toBeNull();
    expect(parseFiniteNumberText(`${"1".repeat(120)}e123456789`)).toBeNull();
  });

  it("accepts complete finite decimal text", () => {
    expect(parseFiniteNumberText("1")).toBe(1);
    expect(parseFiniteNumberText("-1.5")).toBe(-1.5);
    expect(parseFiniteNumberText(".25")).toBe(0.25);
    expect(parseFiniteNumberText("1.")).toBe(1);
    expect(parseFiniteNumberText("1e3")).toBe(1000);
    expect(parseFiniteNumberText("-1.5e-2")).toBe(-0.015);
  });
});

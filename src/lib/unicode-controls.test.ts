import { describe, expect, it } from "vitest";
import * as unicodeControls from "./unicode-controls";
import { hasUnicodeControls, stripUnicodeControls } from "./unicode-controls";

const REJECTED_CODEPOINTS = [
  0x061c, 0x180e, 0x200b, 0x200c, 0x200d, 0x200e, 0x200f, 0x2028, 0x2029, 0x202a, 0x202b, 0x202c,
  0x202d, 0x202e, 0x2060, 0x2066, 0x2067, 0x2068, 0x2069, 0xfeff,
];

describe("unicode-controls", () => {
  it("detects and strips every documented BiDi, zero-width, and line-separator codepoint", () => {
    for (const cp of REJECTED_CODEPOINTS) {
      const char = String.fromCodePoint(cp);
      const wrapped = `prefix${char}suffix`;
      expect(hasUnicodeControls(wrapped), `U+${cp.toString(16).toUpperCase()}`).toBe(true);
      expect(stripUnicodeControls(wrapped), `U+${cp.toString(16).toUpperCase()}`).toBe(
        "prefixsuffix"
      );
    }
  });

  it("leaves ordinary ASCII, CJK, and emoji intact without stateful regex drift", () => {
    const clean = "Episode 01 - 第一话 😀";
    for (let i = 0; i < 5; i += 1) {
      expect(hasUnicodeControls(clean)).toBe(false);
      expect(stripUnicodeControls(clean)).toBe(clean);
      expect(hasUnicodeControls(`a\u202eb`)).toBe(true);
    }
  });

  it("keeps stateful /g regex instances private to the module", () => {
    expect("BIDI_AND_ZERO_WIDTH_GLOBAL_RE" in unicodeControls).toBe(false);
  });
});

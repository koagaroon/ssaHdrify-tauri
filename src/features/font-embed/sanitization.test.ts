/**
 * Cross-helper symmetry pin (Round 2 N-R2-17).
 *
 * font-collector.ts::sanitizeFamily and ass-uuencode.ts::buildFontEntry's
 * inline safeName regex share a documented stripping contract:
 *
 *   sanitizeFamily   strips: [\x00-\x1f, \x7f-\x9f, U+2028, U+2029]
 *   buildFontEntry   strips: [\x00-\x1f, \x7f-\x9f, U+2028, U+2029, :]
 *                            (the extra : is because : is the ASS
 *                            [Fonts] header field separator on the
 *                            fontname: line.)
 *
 * If a future refactor drops part of the shared range from one side
 * without the other, the parity comment in sanitizeFamily's docblock
 * decays silently. These tests pin the contract so the regex stays
 * symmetric on the shared codepoints.
 */
import { describe, it, expect } from "vitest";

import { sanitizeFamily } from "./font-collector";
import { buildFontEntry } from "./ass-uuencode";

// Names containing each character class that must be stripped on both
// sides. The C1 range (U+0080..U+009F) overlaps with several Latin-1
// glyphs visually, so we sample a few representative codepoints in each
// class rather than enumerating all 32 + 32 control codes. Line / para
// separators use \u2028 / \u2029 escape form so eslint's
// no-irregular-whitespace rule passes on this source.
const C0_SAMPLE = "\x00\x01\x09\x1f"; // NUL, SOH, TAB, US
const DEL = "\x7f";
const C1_SAMPLE = "\x80\x9f"; // PAD, APC
const LINE_SEP = "\u2028";
const PARA_SEP = "\u2029";

const FONT_DATA = new Uint8Array([0x00, 0x01, 0x02, 0x03]);

function buildFontEntryName(name: string): string {
  // buildFontEntry returns `fontname: <safeName>\n<encoded>`; pull the
  // safeName slice for direct comparison.
  const entry = buildFontEntry(name, FONT_DATA);
  const header = entry.split("\n", 1)[0]; // "fontname: <safeName>"
  return header.slice("fontname: ".length);
}

describe("font-name sanitization symmetry (Round 2 N-R2-17)", () => {
  it("sanitizeFamily strips C0 + DEL + C1 + line separators", () => {
    const input = `Arial${C0_SAMPLE}${DEL}${C1_SAMPLE}${LINE_SEP}${PARA_SEP}Bold`;
    const out = sanitizeFamily(input);
    expect(out).toBe("ArialBold");
  });

  it("buildFontEntry's safeName replaces the same C0 + DEL + C1 + line-separator range plus :", () => {
    const input = `Arial${C0_SAMPLE}${DEL}${C1_SAMPLE}${LINE_SEP}${PARA_SEP}:Bold`;
    const out = buildFontEntryName(input);
    // Each stripped codepoint becomes `_` (safeName uses replacement,
    // not deletion - preserves visual position in the [Fonts] line).
    const replaceCount = C0_SAMPLE.length + DEL.length + C1_SAMPLE.length + 2 + 1; // +1 for ":"
    expect(out).toBe("Arial" + "_".repeat(replaceCount) + "Bold");
  });

  it("buildFontEntry strips ':' (safeName-only contract)", () => {
    expect(buildFontEntryName("foo:bar")).toBe("foo_bar");
  });

  it("sanitizeFamily keeps ':' (the field-separator contract is buildFontEntry-only)", () => {
    expect(sanitizeFamily("foo:bar")).toBe("foo:bar");
  });

  it("sanitizeFamily caps length at 128 chars", () => {
    const input = "x".repeat(200);
    expect(sanitizeFamily(input).length).toBe(128);
  });

  it("ordinary unicode letters pass through both helpers", () => {
    const input = "微软雅黑 Jose";
    expect(sanitizeFamily(input)).toBe(input);
    expect(buildFontEntryName(input)).toBe(input);
  });
});

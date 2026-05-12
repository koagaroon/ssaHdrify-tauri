/**
 * font-collector regex-shape pins.
 *
 * Direct collection-path tests on `collectFonts` to lock the
 * override-block parser's behavior against subtle regex regressions.
 * Where the parser's mental model could plausibly shift across
 * refactors (e.g., "be more lenient about whitespace in tag syntax"),
 * pin the current libass-matching behavior with a positive assertion
 * on the FontUsage codepoints.
 */
import { describe, it, expect } from "vitest";

import { collectFonts, ensureLoaded } from "./font-collector";

function makeASS(dialogue: string): string {
  return `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,Arial,40,0,0

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Default,${dialogue}
`;
}

describe("font-collector \\p drawing-tag whitespace handling", () => {
  it("rejects `\\p 1` (space before digit) and continues collecting glyphs", async () => {
    // libass requires `\p` followed immediately by a digit — `\p 1`
    // with a space is malformed and does NOT enter drawing mode. The
    // collector regex `\\p(\\d+)` enforces this by requiring `\d`
    // directly after `\p`. If a future "be lenient about whitespace"
    // refactor accepted `\p 1` as a valid drawing-on tag, isDrawing
    // would flip to true and the text after the block would be
    // skipped from glyph collection — the rendered subtitle would
    // then miss those glyphs (renderer + collector disagree).
    //
    // Test: send text starting with `{\p 1}` followed by a sentinel
    // codepoint (Z = U+005A). If the regex correctly rejects, Z is
    // collected; if it falsely accepts, Z is treated as a drawing
    // command and dropped.
    await ensureLoaded();
    const usage = collectFonts(makeASS(String.raw`{\p 1}ZZZZ`));
    const defaultStyle = usage.find((u) => u.key.family === "Arial");
    expect(defaultStyle, "Default style FontUsage should exist").toBeDefined();
    expect(defaultStyle!.codepoints.has(0x5a), "Z (U+005A) must be in collected codepoints").toBe(
      true
    );
  });

  it("accepts `\\p1` (no whitespace) and skips subsequent text as drawing commands", async () => {
    // Counter-test pinning the other direction of the contract:
    // `\p1` (well-formed, scale 1, no whitespace) IS drawing-on per
    // libass, and the collector must skip glyphs until `\p0` or `\r`.
    await ensureLoaded();
    const usage = collectFonts(makeASS(String.raw`{\p1}XXXX{\p0}YYYY`));
    const defaultStyle = usage.find((u) => u.key.family === "Arial");
    expect(defaultStyle).toBeDefined();
    // X (0x58) is inside drawing mode → skipped.
    expect(defaultStyle!.codepoints.has(0x58), "X must NOT be collected (drawing-on)").toBe(false);
    // Y (0x59) is after `\p0` → drawing-off → collected.
    expect(defaultStyle!.codepoints.has(0x59), "Y must be collected (drawing-off)").toBe(true);
  });

  it("multi-\\p block uses LAST tag's drawing state (libass parity)", async () => {
    // Round 4 A-R4-07 / Codex 1: `{\p1\p0}` resolves to drawing-OFF
    // because the LAST `\p` wins. Text after the block is regular
    // glyphs and must be collected. A regression to first-match-only
    // would set drawing-on and skip the text.
    await ensureLoaded();
    const usage = collectFonts(makeASS(String.raw`{\p1\p0}QQQQ`));
    const defaultStyle = usage.find((u) => u.key.family === "Arial");
    expect(defaultStyle).toBeDefined();
    expect(defaultStyle!.codepoints.has(0x51), "Q must be collected (last \\p=0 wins)").toBe(true);
  });
});

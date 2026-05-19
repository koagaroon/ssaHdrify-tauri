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

// ── Round 9 N-R9-N2-3 — last-wins parity for the other 4 override
// tags that Wave 7.5 fixed (\fn / \b / \i / \r). The \p test above
// pins libass parity for one tag; the other four were unanchored,
// so a regression flipping any back to .match() (first-wins) would
// silently mis-attribute fonts / styles between embed and render. ──

describe("font-collector multi-tag last-wins parity (W7.5 regression anchor)", () => {
  it("multi-\\fn block uses LAST family (libass parity)", async () => {
    // `{\fnArial\fnTimes New Roman}` resolves to `Times New Roman` —
    // the last `\fn` wins. A regression to first-match-only would
    // collect glyphs under `Arial` while libass renders with
    // `Times New Roman`, diverging the embedded font from what gets
    // displayed.
    await ensureLoaded();
    const usage = collectFonts(makeASS(String.raw`{\fnArial\fnTimes New Roman}ABCD`));
    const times = usage.find((u) => u.key.family === "Times New Roman");
    const arial = usage.find((u) => u.key.family === "Arial" && u.codepoints.has(0x41));
    expect(times, "Times New Roman FontUsage must exist (last \\fn wins)").toBeDefined();
    expect(times!.codepoints.has(0x41), "A must be collected under Times New Roman").toBe(true);
    expect(arial, "Arial must NOT collect A (it was overridden by the later \\fn)").toBeUndefined();
  });

  it("multi-\\b block uses LAST bold state (libass parity)", async () => {
    // `{\b0\b1}` resolves to bold-ON. A first-wins regression would
    // bucket the text under the Default style (bold=0).
    await ensureLoaded();
    const usage = collectFonts(makeASS(String.raw`{\b0\b1}BBBB`));
    const boldOn = usage.find((u) => u.key.family === "Arial" && u.key.bold && !u.key.italic);
    expect(boldOn, "Arial Bold FontUsage must exist (last \\b1 wins)").toBeDefined();
    expect(boldOn!.codepoints.has(0x42), "B must land in the bold bucket").toBe(true);
  });

  it("multi-\\i block uses LAST italic state (libass parity)", async () => {
    // `{\i1\i0}` resolves to italic-OFF. First-wins regression would
    // bucket under italic=1.
    await ensureLoaded();
    const usage = collectFonts(makeASS(String.raw`{\i1\i0}CCCC`));
    const italicOff = usage.find((u) => u.key.family === "Arial" && !u.key.italic && !u.key.bold);
    expect(italicOff, "Arial non-italic FontUsage must exist (last \\i0 wins)").toBeDefined();
    expect(italicOff!.codepoints.has(0x43), "C must land in the non-italic bucket").toBe(true);
  });

  it("multi-\\r block uses LAST style reset (libass parity)", async () => {
    // `{\rStyleA\rStyleB}` resolves to StyleB. First-wins regression
    // would route the post-block glyphs to StyleA's font. Construct
    // an ASS with two distinct named styles so the test can observe
    // which family the collector picks. The Default style stays Arial;
    // the named styles use Times and Courier so the disambiguation is
    // unambiguous.
    await ensureLoaded();
    const assWithStyles = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,Arial,40,0,0
Style: StyleA,Times New Roman,40,0,0
Style: StyleB,Courier New,40,0,0

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Default,${String.raw`{\rStyleA\rStyleB}DDDD`}
`;
    const usage = collectFonts(assWithStyles);
    const courier = usage.find((u) => u.key.family === "Courier New");
    const times = usage.find((u) => u.key.family === "Times New Roman");
    expect(courier, "Courier New FontUsage must exist (last \\rStyleB wins)").toBeDefined();
    expect(courier!.codepoints.has(0x44), "D must land in StyleB's bucket (Courier New)").toBe(
      true
    );
    // StyleA's family must NOT collect D — it was overridden.
    if (times) {
      expect(times.codepoints.has(0x44), "D must NOT be in Times New Roman").toBe(false);
    }
  });
});

// ── Codex 994c42d1 — boundary-pin for \r and \fn capture caps.
// R2 W2 added {0,127}/{0,128} caps for Pattern 2 symmetry, but
// without a trailing boundary the bounded regex silently TRUNCATED
// overlong names to the prefix, then performed styleMap.has(prefix).
// An attacker-crafted ASS defining both a 128-char prefix style and
// a longer same-prefix style would mis-attribute glyphs to PrefixFont
// while libass renders LongFont — embedded subsets diverge from
// what's drawn. The fix adds a negative-lookahead boundary so
// overlong names fail to match outright and fall through to the
// dialogue's initial style (libass parity for an unknown name).
// Tests below pin both sides of the boundary: at-cap match works,
// over-cap match does NOT mis-attribute. \fn sibling parity is
// tested though it has no demonstrated real-world exploit.
describe("font-collector \\r / \\fn overlong-name boundary (Codex 994c42d1)", () => {
  it("\\r at 128-char cap matches and selects the named style", async () => {
    await ensureLoaded();
    // {0,127} = leading letter + up to 127 continuation chars = 128 total.
    const styleName = "A".repeat(128);
    const ass = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,Arial,40,0,0
Style: ${styleName},Times New Roman,40,0,0

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Default,${String.raw`{\r` + styleName + `}E`}
`;
    const usage = collectFonts(ass);
    const times = usage.find((u) => u.key.family === "Times New Roman");
    expect(times, "128-char style must match \\r at the cap").toBeDefined();
    expect(times!.codepoints.has(0x45), "E must be collected under the 128-char style").toBe(true);
  });

  it("\\r overlong name with prefix-sharing sibling falls through to default", async () => {
    // The Codex PoC: pre-fix the 128-char prefix style would absorb F.
    // Post-fix the overlong \r fails to match, so F stays under the
    // dialogue's initial style (Arial). The 128-char prefix style
    // FontUsage may exist (registered in styleMap) but must NOT carry F.
    await ensureLoaded();
    const prefix = "A".repeat(128);
    const longer = prefix + "B"; // 129 chars sharing the 128-char prefix
    const ass = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,Arial,40,0,0
Style: ${prefix},Times New Roman,40,0,0
Style: ${longer},Courier New,40,0,0

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Default,${String.raw`{\r` + longer + `}F`}
`;
    const usage = collectFonts(ass);
    // F (0x46) MUST land in Arial (initial style), NOT Times New Roman.
    const times = usage.find((u) => u.key.family === "Times New Roman");
    if (times) {
      expect(
        times.codepoints.has(0x46),
        "F must NOT mis-attribute to PrefixFont (Times) — Codex 994c42d1"
      ).toBe(false);
    }
    const arial = usage.find((u) => u.key.family === "Arial");
    expect(arial, "Arial (Default) FontUsage must exist").toBeDefined();
    expect(
      arial!.codepoints.has(0x46),
      "F must land in Arial — overlong \\r falls through to initial style"
    ).toBe(true);
  });

  it("\\fn at 128-char cap matches the family", async () => {
    // Sibling at-cap test. \fn's cap is {0,128} flat (no separate
    // leading-letter requirement), so 128 chars match exactly.
    await ensureLoaded();
    const family = "A".repeat(128);
    const usage = collectFonts(makeASS(`{\\fn${family}}G`));
    const match = usage.find((u) => u.key.family === family);
    expect(match, "128-char family must be captured by \\fn at the cap").toBeDefined();
    expect(match!.codepoints.has(0x47), "G must be collected under the 128-char family").toBe(true);
  });

  it("\\fn overlong name does NOT capture the prefix (sibling parity with \\r)", async () => {
    // Real-world exploit for \fn is much harder than \r (would need
    // two installed fonts with a 128-char shared family-name prefix)
    // but the structural defect is identical. 129-char input must
    // NOT yield a 128-char-prefix capture; the dialogue's initial
    // style stays in force.
    await ensureLoaded();
    const overlong = "A".repeat(129);
    const usage = collectFonts(makeASS(`{\\fn${overlong}}H`));
    const overlongPrefix = "A".repeat(128);
    const prefixHit = usage.find((u) => u.key.family === overlongPrefix);
    expect(
      prefixHit,
      "128-char prefix MUST NOT be captured when input is 129-char (overlong fails to match)"
    ).toBeUndefined();
    const arial = usage.find((u) => u.key.family === "Arial");
    expect(arial, "Arial (Default) FontUsage must exist").toBeDefined();
    expect(arial!.codepoints.has(0x48), "H must land in Arial — overlong \\fn falls through").toBe(
      true
    );
  });
});

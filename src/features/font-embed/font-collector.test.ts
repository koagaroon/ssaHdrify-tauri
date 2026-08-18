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

import { collectFonts, ensureLoaded, fontKeyLabel } from "./font-collector";

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

describe("fontKeyLabel", () => {
  const boldItalic = { family: "Arial", bold: true, italic: true };

  it("uses English style suffixes by default for non-GUI callers", () => {
    expect(fontKeyLabel(boldItalic)).toBe("Arial Bold Italic");
  });

  it("accepts localized style suffixes for GUI callers", () => {
    expect(fontKeyLabel(boldItalic, { bold: "粗体", italic: "斜体" })).toBe("Arial 粗体 斜体");
  });
});

describe("font-collector \\p drawing-tag whitespace handling", () => {
  it("accepts `\\p 1` with leading argument whitespace", async () => {
    await ensureLoaded();
    const usage = collectFonts(makeASS(String.raw`{\p 1}ZZZZ{\p0}Y`));
    const defaultStyle = usage.find((u) => u.key.family === "Arial");
    expect(defaultStyle, "Default style FontUsage should exist").toBeDefined();
    expect(defaultStyle!.codepoints.has(0x5a), "Z must be skipped in drawing mode").toBe(false);
    expect(defaultStyle!.codepoints.has(0x59), "Y after \\p0 must be collected").toBe(true);
  });

  it("accepts `\\p1` (no whitespace) and skips subsequent text as drawing commands", async () => {
    // Counter-test pinning the other direction of the contract:
    // `\p1` (well-formed, scale 1, no whitespace) IS drawing-on per
    // libass, and the collector must skip glyphs until `\p0`.
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
    // `{\p1\p0}` resolves to drawing-OFF because the LAST `\p` wins.
    // Text after the block is regular
    // glyphs and must be collected. A regression to first-match-only
    // would set drawing-on and skip the text.
    await ensureLoaded();
    const usage = collectFonts(makeASS(String.raw`{\p1\p0}QQQQ`));
    const defaultStyle = usage.find((u) => u.key.family === "Arial");
    expect(defaultStyle).toBeDefined();
    expect(defaultStyle!.codepoints.has(0x51), "Q must be collected (last \\p=0 wins)").toBe(true);
  });
});

// ── Last-wins parity for the other 4 override tags (\fn / \b / \i /
// \r). The \p test above pins libass parity for one tag; the other
// four were unanchored, so a regression flipping any back to
// .match() (first-wins) would silently mis-attribute fonts / styles
// between embed and render. ──

describe("font-collector multi-tag last-wins parity", () => {
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

  // ── Overlong numeric tags must parse the FULL digit run, not a
  // bounded prefix. An earlier bounded form for the three numeric
  // tag regexes (\b → \d{1,4}, \i → \d{1,2}, \p → \d{1,4}) without
  // a (?!\d) boundary caused JS global matchAll to capture the
  // leading prefix of any longer digit run. The truncated value
  // then fed parseInt → wrong bold / italic / drawing state,
  // diverging from what ass-compiler (and libass) resolve. The
  // canonical short-form siblings pair with the overlong cases as
  // baseline coherence checks — a re-bounding regression lights up
  // the overlong test (truncated prefix gives the wrong state), and
  // the canonical short-form pin makes sure the regex still handles
  // well-formed short inputs correctly.

  it("\\b00700 (5 digits, overlong) parses as weight=700 → bold", async () => {
    // Truncated `0070` would give weight 70 → NOT bold; full `00700`
    // gives weight 700 → bold-on per libass. B must land in the
    // Arial Bold bucket.
    await ensureLoaded();
    const usage = collectFonts(makeASS(String.raw`{\b00700}BBBB`));
    const boldOn = usage.find((u) => u.key.family === "Arial" && u.key.bold && !u.key.italic);
    expect(boldOn, "Arial Bold FontUsage must exist (full weight=700 parses bold)").toBeDefined();
    expect(boldOn!.codepoints.has(0x42), "B must land in the Arial Bold bucket").toBe(true);
  });

  it("\\b0700 (canonical short form) parses as weight=700 → bold", async () => {
    // Baseline coherence check paired with the overlong sibling
    // above: a future regression that re-narrows the regex to
    // `\d{1,4}` without `(?!\d)` lights up the overlong test
    // (truncated prefix gives weight 70 → NOT bold), while this
    // canonical short-form pin guarantees the regex still resolves
    // well-formed short inputs correctly.
    await ensureLoaded();
    const usage = collectFonts(makeASS(String.raw`{\b0700}BBBB`));
    const boldOn = usage.find((u) => u.key.family === "Arial" && u.key.bold && !u.key.italic);
    expect(boldOn, "Arial Bold FontUsage must exist (weight=700)").toBeDefined();
    expect(boldOn!.codepoints.has(0x42), "B must land in the Arial Bold bucket").toBe(true);
  });

  it("\\i001 (3 digits, overlong) parses as flag=1 → italic", async () => {
    // Truncated `00` would give flag 0 → NOT italic; full `001`
    // gives flag 1 → italic-on per libass.
    await ensureLoaded();
    const usage = collectFonts(makeASS(String.raw`{\i001}CCCC`));
    const italicOn = usage.find((u) => u.key.family === "Arial" && u.key.italic && !u.key.bold);
    expect(italicOn, "Arial Italic FontUsage must exist (full flag=1 parses italic)").toBeDefined();
    expect(italicOn!.codepoints.has(0x43), "C must land in the Arial Italic bucket").toBe(true);
  });

  it("\\p00001 (5 digits, overlong) parses as scale=1 → drawing-on", async () => {
    // Truncated `0000` would give scale 0 → drawing OFF, so the
    // sentinel `X` (0x58) would be collected as a glyph; full `00001`
    // gives scale 1 → drawing ON, so `X` is dropped as a drawing
    // command (not a glyph). `\p0` toggle + `Y` sentinel anchors the
    // FontUsage entry so the negative assertion on `X` is observable
    // (when every text codepoint sits inside drawing mode the
    // collector skips the variant entirely).
    await ensureLoaded();
    const usage = collectFonts(makeASS(String.raw`{\p00001}XXXX{\p0}YYYY`));
    const defaultStyle = usage.find((u) => u.key.family === "Arial");
    expect(defaultStyle, "Default style FontUsage should exist").toBeDefined();
    expect(
      defaultStyle!.codepoints.has(0x58),
      "X must NOT be collected (drawing-on from full \\p00001=1)"
    ).toBe(false);
    expect(defaultStyle!.codepoints.has(0x59), "Y must be collected (drawing-off after \\p0)").toBe(
      true
    );
  });

  it("\\p1 (canonical short form) parses as scale=1 → drawing-on", async () => {
    // Baseline coherence check: well-formed `\p1` is the canonical
    // drawing-on shape; the overlong test above pins that `\p00001`
    // resolves to the SAME state, not the truncated-prefix opposite.
    // Same FontUsage-anchor structure as the overlong test.
    await ensureLoaded();
    const usage = collectFonts(makeASS(String.raw`{\p1}XXXX{\p0}YYYY`));
    const defaultStyle = usage.find((u) => u.key.family === "Arial");
    expect(defaultStyle, "Default style FontUsage should exist").toBeDefined();
    expect(defaultStyle!.codepoints.has(0x58), "X must NOT be collected (drawing-on)").toBe(false);
    expect(defaultStyle!.codepoints.has(0x59), "Y must be collected (drawing-off)").toBe(true);
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

describe("font-collector exact \\r names and \\fn length boundary", () => {
  it("selects a 128-character style exactly after trimming a trailing tab", async () => {
    await ensureLoaded();
    const styleName = "A".repeat(128);
    const ass = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,Arial,40,0,0
Style: ${styleName},Times New Roman,40,0,0

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Default,${`{\\r${styleName}\t}E`}
`;
    const usage = collectFonts(ass);
    const times = usage.find((u) => u.key.family === "Times New Roman");
    expect(times, "128-character style must match exactly").toBeDefined();
    expect(times!.codepoints.has(0x45), "E must be collected under the 128-char style").toBe(true);
  });

  it("does not alias an overlong style name to its shared prefix", async () => {
    await ensureLoaded();
    const prefix = "A".repeat(128);
    const longer = prefix + "B";
    const ass = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,Arial,40,0,0
Style: ${prefix},Times New Roman,40,0,0
Style: ${longer},Courier New,40,0,0

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Default,${String.raw`{\r` + longer + `   }F`}
`;
    const usage = collectFonts(ass);
    const times = usage.find((u) => u.key.family === "Times New Roman");
    if (times) {
      expect(times.codepoints.has(0x46), "F must not use the prefix style").toBe(false);
    }
    const courier = usage.find((u) => u.key.family === "Courier New");
    expect(courier, "an overlong style must not be selected").toBeUndefined();
    const arial = usage.find((u) => u.key.family === "Arial");
    expect(arial).toBeDefined();
    expect(arial!.codepoints.has(0x46)).toBe(true);
  });

  it("accepts a 128-character inline family after trimming leading space and trailing tab", async () => {
    await ensureLoaded();
    const family = "A".repeat(128);
    const usage = collectFonts(makeASS(`{\\fn ${family}\t}G`));
    const match = usage.find((u) => u.key.family === family);
    expect(match, "128-char family must be captured by \\fn at the cap").toBeDefined();
    expect(match!.codepoints.has(0x47), "G must be collected under the 128-char family").toBe(true);
  });

  it("does not alias an overlong inline family to its 128-character prefix", async () => {
    await ensureLoaded();
    const overlong = "A".repeat(129);
    const usage = collectFonts(makeASS(`{\\fn${overlong}   }H`));
    const overlongPrefix = "A".repeat(128);
    const prefixHit = usage.find((u) => u.key.family === overlongPrefix);
    expect(prefixHit, "an overlong family must not alias its sanitized prefix").toBeUndefined();
    const arial = usage.find((u) => u.key.family === "Arial");
    expect(arial, "Arial (Default) FontUsage must exist").toBeDefined();
    expect(arial!.codepoints.has(0x48), "H must land in Arial — overlong \\fn falls through").toBe(
      true
    );
  });
});

describe("font-collector \\r / \\fn overlong state-retention", () => {
  it("\\r overlong after a valid prior \\r resets to initial style", async () => {
    // PoC: `{\rStyleA\r<overlong>}X` — the FIRST tag sets state to
    // StyleA, the SECOND (overlong) must reset to dialogue initial per
    // libass semantics. Pre-fix the overlong didn't match at all, so
    // StyleA's state stayed in force and X was attributed to Times
    // New Roman instead of Arial (the dialogue's initial family).
    await ensureLoaded();
    const overlong = "A".repeat(129);
    const ass = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,Arial,40,0,0
Style: StyleA,Times New Roman,40,0,0

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Default,${String.raw`{\rStyleA\r` + overlong + `}X`}
`;
    const usage = collectFonts(ass);
    const times = usage.find((u) => u.key.family === "Times New Roman");
    if (times) {
      expect(
        times.codepoints.has(0x58),
        "X must NOT remain in StyleA bucket — overlong \\r must reset state"
      ).toBe(false);
    }
    const arial = usage.find((u) => u.key.family === "Arial");
    expect(arial, "Arial (Default/initial) FontUsage must exist").toBeDefined();
    expect(
      arial!.codepoints.has(0x58),
      "X must land in Arial after overlong \\r resets to initial style"
    ).toBe(true);
  });

  it("resets only exact \\fn0 before leading whitespace is removed", async () => {
    await ensureLoaded();
    const usage = collectFonts(makeASS(String.raw`{\fn 0}J{\fn0}K`));
    expect(usage.find((entry) => entry.key.family === "0")?.codepoints.has(0x4a)).toBe(true);
    expect(usage.find((entry) => entry.key.family === "Arial")?.codepoints.has(0x4b)).toBe(true);
  });

  it("\\fn overlong after a valid prior \\fn resets to initial family", async () => {
    // Sibling PoC: `{\fnTimes New Roman\fn<overlong>}Y` — the FIRST
    // tag sets family to Times, the SECOND (overlong) must reset to
    // dialogue initial family per libass semantics. Pre-fix the
    // overlong didn't match, so Times stayed in force and Y was
    // attributed to Times instead of Arial.
    await ensureLoaded();
    const overlong = "A".repeat(129);
    const usage = collectFonts(makeASS(`{\\fnTimes New Roman\\fn${overlong}}Y`));
    const times = usage.find((u) => u.key.family === "Times New Roman");
    if (times) {
      expect(
        times.codepoints.has(0x59),
        "Y must NOT remain in Times bucket — overlong \\fn must reset family"
      ).toBe(false);
    }
    const arial = usage.find((u) => u.key.family === "Arial");
    expect(arial, "Arial (Default/initial) FontUsage must exist").toBeDefined();
    expect(
      arial!.codepoints.has(0x59),
      "Y must land in Arial after overlong \\fn resets to initial family"
    ).toBe(true);
  });

  it("\\fn spoofing whitespace is sanitized from the full family, not captured as a prefix", async () => {
    await ensureLoaded();
    const nbsp = String.fromCharCode(0x00a0);
    const usage = collectFonts(makeASS(`{\\fnArial${nbsp}Black}Z`));

    const sanitized = usage.find((u) => u.key.family === "ArialBlack");
    const prefix = usage.find((u) => u.key.family === "Arial");
    expect(
      sanitized,
      "NBSP-bearing family should sanitize to the full ArialBlack name"
    ).toBeDefined();
    expect(sanitized!.codepoints.has(0x5a), "Z must land in the sanitized full family").toBe(true);
    expect(prefix, "NBSP must not terminate \\fn into the prefix family Arial").toBeUndefined();
  });

  it("\\fn astral codepoint is sanitized from the full family, not captured as a prefix", async () => {
    await ensureLoaded();
    const astral = String.fromCodePoint(0x10000);
    const usage = collectFonts(makeASS(`{\\fnPrefix${astral}Suffix}Q`));

    const sanitized = usage.find((u) => u.key.family === "PrefixSuffix");
    const prefix = usage.find((u) => u.key.family === "Prefix");
    expect(
      sanitized,
      "astral-bearing family should sanitize to the full PrefixSuffix name"
    ).toBeDefined();
    expect(sanitized!.codepoints.has(0x51), "Q must land in the sanitized full family").toBe(true);
    expect(prefix, "astral codepoint must not terminate \\fn into a prefix family").toBeUndefined();
  });

  it("\\fn with only sanitized-away characters resets to the initial family", async () => {
    await ensureLoaded();
    const nbsp = String.fromCharCode(0x00a0);
    const usage = collectFonts(makeASS(`{\\fn${nbsp}}N`));

    const arial = usage.find((u) => u.key.family === "Arial");
    expect(arial, "empty-after-sanitize \\fn must fall back to the initial family").toBeDefined();
    expect(arial!.codepoints.has(0x4e), "N must land in Arial").toBe(true);
  });
});

describe("font-collector \\r digit-led style name", () => {
  it("\\r1MainTitle resolves to the digit-led style when defined", async () => {
    // ass-compiler accepts `Style: 1MainTitle,...`; our \r regex
    // must agree. `{\rStyleA\r1MainTitle}X` should switch to
    // 1MainTitle's font (Courier New here), NOT retain StyleA
    // (Times New Roman) from the prior tag.
    await ensureLoaded();
    const ass = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,Arial,40,0,0
Style: StyleA,Times New Roman,40,0,0
Style: 1MainTitle,Courier New,40,0,0

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Default,${String.raw`{\rStyleA\r1MainTitle}X`}
`;
    const usage = collectFonts(ass);
    const courier = usage.find((u) => u.key.family === "Courier New");
    expect(
      courier,
      "Courier New FontUsage must exist — \\r1MainTitle must switch to the digit-led style"
    ).toBeDefined();
    expect(courier!.codepoints.has(0x58), "X must land in 1MainTitle's bucket (Courier New)").toBe(
      true
    );
    const times = usage.find((u) => u.key.family === "Times New Roman");
    if (times) {
      expect(
        times.codepoints.has(0x58),
        "X must NOT remain in StyleA bucket (Times) — \\r1MainTitle must override"
      ).toBe(false);
    }
  });

  it("\\r9NonexistentStyle (digit-led, undefined) falls through to initial style", async () => {
    // Strict lookup of an unknown named reset returns to the event style.
    await ensureLoaded();
    const ass = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,Arial,40,0,0
Style: StyleA,Times New Roman,40,0,0

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Default,${String.raw`{\rStyleA\r9NonexistentStyle}Z`}
`;
    const usage = collectFonts(ass);
    const times = usage.find((u) => u.key.family === "Times New Roman");
    if (times) {
      expect(
        times.codepoints.has(0x5a),
        "Z must NOT remain in StyleA bucket — undefined digit-led \\r must reset to initial"
      ).toBe(false);
    }
    const arial = usage.find((u) => u.key.family === "Arial");
    expect(arial, "Arial (Default/initial) FontUsage must exist").toBeDefined();
    expect(
      arial!.codepoints.has(0x5a),
      "Z must land in Arial after undefined digit-led \\r resets to initial"
    ).toBe(true);
  });
});

describe("font-collector libass style baselines", () => {
  it("resets bare and invalid bold/italic tags to the event style", async () => {
    await ensureLoaded();
    const ass = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,Arial,40,-1,-1

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Default,${String.raw`{\fnTimes New Roman\fn0\b0\b2\i0\i-1}X`}
`;
    const usage = collectFonts(ass);
    const restored = usage.find(
      (entry) => entry.key.family === "Arial" && entry.key.bold && entry.key.italic
    );
    expect(restored).toBeDefined();
    expect(restored!.codepoints.has(0x58)).toBe(true);
  });

  it("uses the latest named style for \\fn0 and invalid bold/italic resets", async () => {
    await ensureLoaded();
    const ass = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,Arial,40,0,0
Style: Named,Courier New,40,-1,-1

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Default,${String.raw`{\rNamed\fnTimes New Roman\fn0\b0\b99\i0\i2}Y`}
`;
    const usage = collectFonts(ass);
    const restored = usage.find(
      (entry) => entry.key.family === "Courier New" && entry.key.bold && entry.key.italic
    );
    expect(restored, "all resets must use Named rather than Default").toBeDefined();
    expect(restored!.codepoints.has(0x59)).toBe(true);
  });

  it("returns style-relative resets to the event baseline after bare \\r", async () => {
    await ensureLoaded();
    const ass = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,Arial,40,0,0
Style: Named,Courier New,40,-1,-1

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Default,${String.raw`{\rNamed\r\fnTimes\fn0\b1\b2\i1\i2}S`}
`;
    const usage = collectFonts(ass);
    const event = usage.find(
      (entry) => entry.key.family === "Arial" && !entry.key.bold && !entry.key.italic
    );
    expect(event?.codepoints.has(0x53)).toBe(true);
  });

  it("resets literal bare and tab-only bold/italic tags to the named style", async () => {
    await ensureLoaded();
    const bare = String.raw`{\rNamed\b0\i0\b\i}V`;
    const tabOnly = "{\\b0\\i0\\b\t\\i\t}W";
    const ass = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,Arial,40,0,0
Style: Named,Courier New,40,-1,-1

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Default,${bare}${tabOnly}
Dialogue: 0,0:00:00.00,0:00:05.00,Default,${String.raw`{\b1\i1\b\i}R`}
`;
    const usage = collectFonts(ass);
    const named = usage.find(
      (entry) => entry.key.family === "Courier New" && entry.key.bold && entry.key.italic
    );
    expect(named?.codepoints.has(0x56)).toBe(true);
    expect(named?.codepoints.has(0x57)).toBe(true);
    const regular = usage.find(
      (entry) => entry.key.family === "Arial" && !entry.key.bold && !entry.key.italic
    );
    expect(regular?.codepoints.has(0x52), "bare tags must also restore a regular style").toBe(true);
  });

  it("treats present nonnumeric bold/italic arguments as zero", async () => {
    await ensureLoaded();
    const ass = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,Arial,40,-1,-1

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Default,${String.raw`{\bbanana\iabc}Z`}
`;
    const usage = collectFonts(ass);
    const regular = usage.find(
      (entry) => entry.key.family === "Arial" && !entry.key.bold && !entry.key.italic
    );
    expect(regular).toBeDefined();
    expect(regular!.codepoints.has(0x5a)).toBe(true);
  });

  it("accepts leading argument whitespace for bold and italic", async () => {
    await ensureLoaded();
    const usage = collectFonts(makeASS(String.raw`{\b 1\i 1}W`));
    const styled = usage.find(
      (entry) => entry.key.family === "Arial" && entry.key.bold && entry.key.italic
    );
    expect(styled).toBeDefined();
    expect(styled!.codepoints.has(0x57)).toBe(true);
  });

  it("looks up Default and lowercase default exactly", async () => {
    await ensureLoaded();
    const ass = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,Arial,40,0,0
Style: default,Courier New,40,0,0
Style: EventStyle,Times New Roman,40,0,0

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,EventStyle,${String.raw`{\rDefault}A{\rdefault}B{\rDEFAULT}C{\r}D`}
Dialogue: 0,0:00:00.00,0:00:05.00,default,E
Dialogue: 0,0:00:00.00,0:00:05.00,***EventStyle,F
Dialogue: 0,0:00:00.00,0:00:05.00,Default,${String.raw`{\r***EventStyle}G`}
`;
    const usage = collectFonts(ass);
    expect(usage.find((entry) => entry.key.family === "Arial")?.codepoints.has(0x41)).toBe(true);
    expect(usage.find((entry) => entry.key.family === "Courier New")?.codepoints.has(0x42)).toBe(
      true
    );
    const event = usage.find((entry) => entry.key.family === "Times New Roman");
    expect(event?.codepoints.has(0x43), "unknown uppercase name must use the event style").toBe(
      true
    );
    expect(event?.codepoints.has(0x44), "bare reset must use the event style").toBe(true);
    expect(
      usage.find((entry) => entry.key.family === "Arial")?.codepoints.has(0x45),
      "event-style default is normalized to exact Default"
    ).toBe(true);
    expect(event?.codepoints.has(0x46), "all leading event-style stars are ignored").toBe(true);
    expect(
      usage.find((entry) => entry.key.family === "Arial")?.codepoints.has(0x47),
      "\\r lookup remains strict and does not strip stars"
    ).toBe(true);
  });

  it("accepts spaces and punctuation and trims trailing tag spaces", async () => {
    await ensureLoaded();
    const ass = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,Arial,40,0,0
Style: Signs - Top! #1,Courier New,40,0,0

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Default,${String.raw`{\rSigns - Top! #1   \b1}Q`}
`;
    const usage = collectFonts(ass);
    const signs = usage.find((entry) => entry.key.family === "Courier New" && entry.key.bold);
    expect(signs).toBeDefined();
    expect(signs!.codepoints.has(0x51)).toBe(true);
  });

  it("falls back from an undefined event style to Default, then Arial", async () => {
    await ensureLoaded();
    const withDefault = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,Courier New,40,0,0

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Missing,X
`;
    expect(
      collectFonts(withDefault)
        .find((entry) => entry.key.family === "Courier New")
        ?.codepoints.has(0x58)
    ).toBe(true);

    const withoutDefault = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Other,Times New Roman,40,0,0

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Missing,Y
`;
    expect(
      collectFonts(withoutDefault)
        .find((entry) => entry.key.family === "Arial")
        ?.codepoints.has(0x59)
    ).toBe(true);
  });
});

describe("font-collector \\r and drawing-mode state", () => {
  it("switches style without ending drawing mode", async () => {
    await ensureLoaded();
    const ass = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,Arial,40,0,0
Style: StyleA,Courier New,40,0,0

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Default,${String.raw`{\p1}{\rStyleA}m 0 0 l 1 1{\p0}X`}
`;
    const usage = collectFonts(ass);
    const courier = usage.find((u) => u.key.family === "Courier New");
    expect(courier).toBeDefined();
    expect(courier!.codepoints.has(0x6d), "drawing command m must remain skipped").toBe(false);
    expect(courier!.codepoints.has(0x58), "X after \\p0 must use StyleA").toBe(true);
  });

  it("unknown \\r falls back to the event style while preserving drawing mode", async () => {
    await ensureLoaded();
    const usage = collectFonts(makeASS(String.raw`{\p1}{\r9NonexistentStyle}m 0 0 l 1 1{\p0}Y`));
    const arial = usage.find((u) => u.key.family === "Arial");
    expect(arial).toBeDefined();
    expect(arial!.codepoints.has(0x6d)).toBe(false);
    expect(arial!.codepoints.has(0x59)).toBe(true);
  });
});

describe("font-collector left-to-right override order", () => {
  it("{\\fnTimes\\r}X resets family to initial — \\fn-then-\\r positional order", async () => {
    // \r resets font styling to the event baseline, but drawing mode is
    // intentionally tested separately because libass preserves it.
    await ensureLoaded();
    const usage = collectFonts(makeASS(String.raw`{\fnTimes New Roman\r}X`));
    const times = usage.find((u) => u.key.family === "Times New Roman");
    if (times) {
      expect(
        times.codepoints.has(0x58),
        "X must NOT land in Times — \\r after \\fn must reset family to initial"
      ).toBe(false);
    }
    const arial = usage.find((u) => u.key.family === "Arial");
    expect(arial, "Arial (Default/initial) FontUsage must exist").toBeDefined();
    expect(
      arial!.codepoints.has(0x58),
      "X must land in Arial — \\r resets family to dialogue initial"
    ).toBe(true);
  });

  it("{\\b1\\r}X resets bold to initial — \\b-then-\\r positional order", async () => {
    // libass: \b1 sets bold-on, then \r resets to dialogue initial
    // (Default style is bold=0). X collected under Arial NON-bold,
    // not Arial-Bold. Before the position-sorted fix: \b
    // matchAll.at(-1) ran independently and set bold=true regardless
    // of \r's relative position.
    await ensureLoaded();
    const usage = collectFonts(makeASS(String.raw`{\b1\r}X`));
    const arialBold = usage.find((u) => u.key.family === "Arial" && u.key.bold);
    if (arialBold) {
      expect(
        arialBold.codepoints.has(0x58),
        "X must NOT land in Arial-Bold — \\r after \\b1 must reset bold to initial"
      ).toBe(false);
    }
    const arialPlain = usage.find((u) => u.key.family === "Arial" && !u.key.bold && !u.key.italic);
    expect(arialPlain, "Arial non-bold FontUsage must exist").toBeDefined();
    expect(arialPlain!.codepoints.has(0x58), "X must land in Arial non-bold").toBe(true);
  });

  it("{\\i1\\r}X resets italic to initial — \\i-then-\\r positional order", async () => {
    // Sibling-parity with \b1\r above. Default style is italic=0.
    await ensureLoaded();
    const usage = collectFonts(makeASS(String.raw`{\i1\r}X`));
    const arialItalic = usage.find((u) => u.key.family === "Arial" && u.key.italic);
    if (arialItalic) {
      expect(
        arialItalic.codepoints.has(0x58),
        "X must NOT land in Arial-Italic — \\r after \\i1 must reset italic to initial"
      ).toBe(false);
    }
    const arialPlain = usage.find((u) => u.key.family === "Arial" && !u.key.bold && !u.key.italic);
    expect(arialPlain, "Arial non-italic FontUsage must exist").toBeDefined();
    expect(arialPlain!.codepoints.has(0x58), "X must land in Arial non-italic").toBe(true);
  });

  it("{\\p1\\r} preserves drawing mode until \\p0", async () => {
    await ensureLoaded();
    const usage = collectFonts(makeASS(String.raw`{\p1\r}m 0 0 l 1 1{\p0}X`));
    const arial = usage.find((u) => u.key.family === "Arial");
    expect(arial).toBeDefined();
    expect(arial!.codepoints.has(0x6d), "drawing command m must stay skipped after \\r").toBe(
      false
    );
    expect(arial!.codepoints.has(0x58), "X after \\p0 must be collected").toBe(true);
  });

  it("{\\r\\fnTimes}X — \\r-then-\\fn keeps the later \\fn family (libass parity, sanity check)", async () => {
    // Counter-direction: when \fn comes AFTER \r in source order,
    // \fn correctly overrides the reset's family choice. This was
    // already the behavior before the position-sorted refactor (\r
    // matchAll then \fn matchAll gave the same end state), but pin
    // it explicitly so a future refactor that flips the
    // position-walk direction doesn't silently regress.
    await ensureLoaded();
    const usage = collectFonts(makeASS(String.raw`{\r\fnTimes New Roman}X`));
    const times = usage.find((u) => u.key.family === "Times New Roman");
    expect(
      times,
      "Times New Roman FontUsage must exist — \\fn after \\r overrides reset's family"
    ).toBeDefined();
    expect(
      times!.codepoints.has(0x58),
      "X must land in Times — \\fn after \\r wins per left-to-right"
    ).toBe(true);
  });
});

describe("font-collector long \\r arguments", () => {
  it("consumes a 200000-character name as an event-style fallback", async () => {
    await ensureLoaded();
    const longName = "A".repeat(200000);
    const usage = collectFonts(makeASS(String.raw`{\fnTimes\r` + longName + String.raw`}F`));
    expect(usage.find((entry) => entry.key.family === "Arial")?.codepoints.has(0x46)).toBe(true);
  });

  it("also consumes longer names instead of retaining a prior override", async () => {
    await ensureLoaded();
    const tooLong = "A".repeat(200001);
    const usage = collectFonts(makeASS(String.raw`{\fnTimes\r` + tooLong + String.raw`}F`));
    expect(usage.find((entry) => entry.key.family === "Arial")?.codepoints.has(0x46)).toBe(true);
  });
});

describe("font-collector per-variant codepoint cap boundary", () => {
  function makeUniqueGlyphRun(count: number): string {
    return Array.from({ length: count }, (_, i) => String.fromCodePoint(0x10000 + i)).join("");
  }

  it("allows duplicate glyphs after the per-variant cap is exactly full", async () => {
    await ensureLoaded();
    const atCap = makeUniqueGlyphRun(65536);
    const usage = collectFonts(makeASS(`${atCap}${String.fromCodePoint(0x10000)}`));
    const arial = usage.find((u) => u.key.family === "Arial");
    expect(arial?.codepoints.size).toBe(65536);
  });

  it("rejects a new glyph just beyond the per-variant cap", async () => {
    await ensureLoaded();
    const overCap = makeUniqueGlyphRun(65537);
    expect(() => collectFonts(makeASS(overCap))).toThrow(/Too many codepoints for one font/);
  });
});

describe("font-collector font-variant + total-codepoint cap boundaries", () => {
  function makeUniqueGlyphRun(count: number): string {
    return Array.from({ length: count }, (_, i) => String.fromCodePoint(0x10000 + i)).join("");
  }
  // `count` distinct \fn families, each used with one glyph → `count`
  // usageMap entries. No leading text before the first \fn, so the style's
  // default font is never recorded as an extra variant.
  function makeVariants(count: number): string {
    return Array.from({ length: count }, (_, i) => `{\\fnFam${i}}x`).join("");
  }
  // One Dialogue line per entry — used to spread codepoints across lines so
  // each stays under the per-dialogue text cap (MAX_DIALOGUE_TEXT_LEN) while
  // the function-level codepoint total still accumulates across them.
  function makeMultiDialogueAss(dialogues: string[]): string {
    return [
      "[Script Info]",
      "ScriptType: v4.00+",
      "",
      "[V4+ Styles]",
      "Format: Name, Fontname, Fontsize, Bold, Italic",
      "Style: Default,Arial,40,0,0",
      "",
      "[Events]",
      "Format: Layer, Start, End, Style, Text",
      ...dialogues.map((d) => `Dialogue: 0,0:00:00.00,0:00:05.00,Default,${d}`),
      "",
    ].join("\n");
  }

  it("allows exactly MAX_FONT_VARIANTS (500) distinct font variants", async () => {
    await ensureLoaded();
    expect(collectFonts(makeASS(makeVariants(500)))).toHaveLength(500);
  });

  it("rejects the 501st font variant", async () => {
    await ensureLoaded();
    expect(() => collectFonts(makeASS(makeVariants(501)))).toThrow(/Too many font variants/);
  });

  it("rejects when the cross-variant codepoint total exceeds MAX_TOTAL_CODEPOINTS", async () => {
    await ensureLoaded();
    // 16 variants × 65,536 glyphs = 1,048,576 > the 1,000,000 total cap.
    // Spread one variant per Dialogue line so each line stays under the
    // per-dialogue text cap (a single 16×65,536 line would trip
    // MAX_DIALOGUE_TEXT_LEN first); each variant sits AT the per-variant cap,
    // and 16 variants is well under MAX_FONT_VARIANTS — isolating the
    // total-codepoint guard, which sits adjacent to the per-variant one.
    const run = makeUniqueGlyphRun(65536);
    const ass = makeMultiDialogueAss(Array.from({ length: 16 }, (_, i) => `{\\fnFam${i}}${run}`));
    expect(() => collectFonts(ass)).toThrow(/Too many codepoints across fonts/);
  });
});

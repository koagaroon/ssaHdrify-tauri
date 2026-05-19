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

// ── Codex f871d0cc — state-retention pair for the 994c42d1 boundary fix.
// 994c42d1's boundary lookahead alone made overlong \r / \fn silently
// disappear from matchAll. The if-block at the caller doesn't execute
// when matchAll returns no token, so a PRIOR valid \r / \fn in the same
// override block leaves its state in `result` — X / Y get attributed to
// the prior style / family instead of being reset to the dialogue
// initial (libass semantics). R4 W1 fixed it via a second alternation
// that matches overlong runs with undefined capture, so the if-block
// runs and falls through to the initialFont path. These tests pin the
// state-retention contract specifically, complementing the 994c42d1
// boundary tests above which only cover the no-prior-override case.
describe("font-collector \\r / \\fn overlong state-retention (Codex f871d0cc)", () => {
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
});

// ── R5 W1 (A-R5-1) — digit-led style name pair for the f871d0cc
// state-retention fix. R4 W1's alternation closed the overlong-run
// shape but kept the original `[\p{L}_]` leading-char class on BOTH
// branches, so a digit-led style name (`1MainTitle`) still failed both
// alternation branches — same state-retention divergence in a
// different input shape. ass-compiler accepts digit-led names and
// stores them in styleMap unchanged; the parser-vs-override-tag
// asymmetry was an internal inconsistency regardless of libass
// behavior. Fix extends both leading classes to `[\p{L}\p{N}_]`.
// Tests below cover both directions: digit-led name DEFINED in
// V4+ Styles (must switch to that style) and digit-led name
// UNDEFINED (must reset to initial, matching the overlong path).
describe("font-collector \\r digit-led style name (A-R5-1)", () => {
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
    // Sibling counter-test for the undefined case: `\r<digit-led-name>`
    // where the name is NOT in styleMap. Pre-R5-W1 fix, both alternation
    // branches rejected the digit-led leading char, so matchAll missed
    // the token and prior state stayed in force. Post-fix, the first
    // branch matches with capture = "9NonexistentStyle", styleMap.has
    // returns false, the else-arm resets to initialFont.
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

// ── R6 W1 (N-R6-1 / A-R6-1) — drawing-reset parity for digit-led \r.
// R5 W1 widened the \r alternation regex inside applyOverrideTags to
// accept digit-led style names, but the SIBLING regex R_RESET_RE at
// module top (which the walkText loop uses to decide whether \r was
// present → reset isDrawing) still used the old [\p{L}_] leading class.
// When an attacker-controlled ASS pairs \p1 with \r<digit-led>, the two
// regexes disagree: applyOverrideTags switches style correctly but
// R_RESET_RE.test() returns false, so isDrawing stays true from the
// prior \p1. Plain text after the block becomes drawing-mode commands
// → glyphs missing from the embedded subset. libass renders normally
// (\r resets drawing-off). Same regex-pair coherence failure mode the
// R5 W1 WHY comment predicted; the hiding spot was 458 lines above in
// the same file.
//
// Tests below pair the two halves of the contract:
//   1. Drawing-mode reset fires for digit-led \r (so subsequent text
//      is collected, not skipped).
//   2. Style-undefined fallback to initialFont still works in
//      combination with the drawing reset.
describe("font-collector \\r digit-led drawing-mode reset (A-R6-1)", () => {
  it("\\p1 in one block, \\r1MainTitle in the next: digit-led \\r resets drawing AND switches style", async () => {
    // Two-block sequence: {\p1}{\r1MainTitle}X. isDrawing is a sticky
    // state that persists across blocks until reset. Block 1's \p1
    // sets isDrawing=true; block 2's \r1MainTitle must reset
    // isDrawing=false (via R_RESET_RE.test) AND switch to 1MainTitle
    // (via applyOverrideTags's matchAll). Then X is collected under
    // Courier New.
    //
    // Pre-R6-W1: R_RESET_RE used [\p{L}_] which rejected the digit
    // leading char; .test returned false; isDrawing stayed true from
    // block 1; X was treated as drawing-mode commands and DROPPED.
    // applyOverrideTags still switched style correctly (R5 W1 widened
    // its alternation regex), but Courier New's bucket would be empty
    // because no glyphs reached recordChars.
    //
    // Single-block {\p1\r1MainTitle}X is NOT used here — the existing
    // walkText drawing-mode pass runs pTags AFTER R_RESET_RE, so the
    // \p1 in the same block always wins regardless of R_RESET_RE's
    // result. That positional-order defect is A-R6-2 / R6 W2's
    // refactor target; this W1-only test must isolate the R_RESET_RE
    // contract via cross-block state propagation.
    await ensureLoaded();
    const ass = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,Arial,40,0,0
Style: 1MainTitle,Courier New,40,0,0

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Default,${String.raw`{\p1}{\r1MainTitle}X`}
`;
    const usage = collectFonts(ass);
    const courier = usage.find((u) => u.key.family === "Courier New");
    expect(
      courier,
      "Courier New FontUsage must exist — block-2 R_RESET_RE must reset isDrawing for digit-led \\r"
    ).toBeDefined();
    expect(
      courier!.codepoints.has(0x58),
      "X must land in 1MainTitle's bucket (Courier New) — proves block-2 \\r1MainTitle both switched style AND reset isDrawing"
    ).toBe(true);
  });

  it("\\p1 in one block, \\r9Nonexistent in the next: digit-led undefined \\r resets drawing AND falls through to initial", async () => {
    // Sibling counter-test: {\p1}{\r9Nonexistent}Y. Block 1 sets
    // isDrawing=true. Block 2's \r9Nonexistent must reset isDrawing
    // (via R_RESET_RE.test) AND fall through to initialFont (style
    // name not in styleMap). Y is collected under Arial.
    //
    // Pre-R6-W1 + R5-W1: applyOverrideTags's alternation rejected
    // digit-led → state retained block 1's style. Pre-R6-W1 alone:
    // R_RESET_RE rejected digit-led → isDrawing stayed true → Y
    // dropped. Post-R6-W1 + R5-W1: both regexes accept digit-led;
    // style resets to initial AND isDrawing resets; Y collected
    // under Arial.
    await ensureLoaded();
    const ass = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,Arial,40,0,0
Style: StyleA,Times New Roman,40,0,0

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Default,${String.raw`{\p1}{\r9NonexistentStyle}Y`}
`;
    const usage = collectFonts(ass);
    const arial = usage.find((u) => u.key.family === "Arial");
    expect(arial, "Arial (Default/initial) FontUsage must exist").toBeDefined();
    expect(
      arial!.codepoints.has(0x59),
      "Y must land in Arial — proves block-2 R_RESET_RE reset isDrawing for digit-led \\r AND undefined-style fall-through fired"
    ).toBe(true);
  });
});

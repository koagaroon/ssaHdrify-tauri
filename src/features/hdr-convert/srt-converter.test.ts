/**
 * SRT/SUB converter tests — ported from Python tests/test_hdrify.py
 *
 * Tests cover: HTML font color preprocessing, ASS document building,
 * format detection helpers, and custom style configuration.
 */
import { describe, it, expect } from "vitest";
import {
  escapeSrtUserText,
  preprocessSrtColors,
  processSrtUserText,
  buildAssDocument,
  isNativeAss,
  isConvertible,
  DEFAULT_STYLE,
} from "./srt-converter";

// ── preprocessSrtColors ──────────────────────────────────

describe("preprocessSrtColors", () => {
  it("converts <font color> to ASS inline override", () => {
    const result = preprocessSrtColors('<font color="#FF0000">Red text</font>');
    // Pin the actual BGR-converted value: SRT color is RGB hex
    // (#FF0000 = red), ASS \1c is BGR hex (&H0000FF). A bare match on
    // /\\1?c&H/ would pass even if the converter emitted &H000000.
    expect(result).toMatch(/\{\\1c&H0000FF&?\}/);
    expect(result).toContain("Red text");
    // HTML tag should be gone
    expect(result).not.toContain("<font");
  });

  it("inserts color reset after </font>", () => {
    const result = preprocessSrtColors('<font color="#FF0000">Red</font> normal');
    // Pin the reset shape — the loose alternation `\{\\1c&H[0-9A-F]{6}\}`
    // would still pass for an emitted `&HDEADBE` (any 6 hex). The reset
    // contract is "either ASS reset tag {\r}, OR explicit BGR black
    // (000000)." Anchor on those two specific shapes so a regression
    // emitting a stray non-zero color still fails.
    expect(result).toMatch(/Red(?:\{\\r\}|\{\\1c&H0+&?\}) normal/);
  });

  it("handles multiple attributes on <font> tag", () => {
    const result = preprocessSrtColors('<font face="Arial" color="#00FF00">Green</font>');
    // RGB #00FF00 → BGR &H00FF00 (mirror of the red case above).
    expect(result).toMatch(/\{\\1c&H00FF00&?\}/);
    expect(result).toContain("Green");
  });

  it("passes through text without font tags unchanged", () => {
    const input = "Just plain text with no HTML";
    expect(preprocessSrtColors(input)).toBe(input);
  });

  it("handles text with no color attribute on font tag", () => {
    const input = '<font face="Arial">Styled</font>';
    const result = preprocessSrtColors(input);
    // Layer-of-responsibility note: preprocessSrtColors only handles
    // color-bearing <font> tags here. The opener without color is
    // intentionally left in — it's stripped downstream by
    // buildAssDocument's HTML-tag removal pass (see srt-converter.ts
    // line 64 comment). The </font> closer DOES become {\\r} regardless,
    // because that path is a uniform style-reset emitter.
    expect(result).toContain("Styled");
    // Closer always becomes {\r}, including when the opener carried no color.
    expect(result).not.toContain("</font>");
    expect(result).toContain("{\\r}");
  });

  it("processes a 100KB pathological near-match payload in linear time", () => {
    // ReDoS regression. SRT_COLOR_OPEN_RE has two `[^>]{0,512}` windows
    // anchored on `>`. JS regex engines are linear on `>`-anchored
    // character-class repetitions, so the worst case should be O(N).
    // Build a 100KB payload with crafted near-matches that would
    // exercise the worst alt-path: many "<font" prefixes that almost
    // satisfy the color-attribute branch but fail at the closing
    // delimiter, forcing backtrack-style behavior in a vulnerable
    // engine. If this hangs the test runner (default 5s vitest
    // timeout), we have a real ReDoS.
    const chunk = '<font face="Arial" data-extra="x"'.repeat(40); // ~1300 bytes
    const payload = chunk.repeat(80) + ">Styled</font>"; // ~100KB
    const start = Date.now();
    const result = preprocessSrtColors(payload);
    const elapsed = Date.now() - start;
    // Generous budget — a working linear-time regex finishes in <100ms;
    // a quadratic backtracker would blow past 5s on this payload.
    expect(elapsed).toBeLessThan(2000);
    expect(typeof result).toBe("string");
  });
});

// ── processSrtUserText (composed entry point) ───────────

describe("processSrtUserText", () => {
  it("escapes user braces before injecting our color tags", () => {
    // Hostile-looking input: a literal `{\\an8}` ASS override embedded in
    // user text plus a real <font color> tag. The composed entry point
    // must escape the user's braces FIRST, then inject our trusted color
    // tag — otherwise the override would survive as a libass directive.
    const input = '{\\an8}<font color="#FF0000">Red</font>';
    const out = processSrtUserText(input);
    // User's `{` got escaped — no raw `{\` of user origin remains.
    expect(out).toContain("\\{\\\\an8\\}");
    // Our injected color tag DID land in the output (real `{\1c…}`).
    expect(out).toMatch(/\{\\1c&H/);
  });

  it("equals preprocessSrtColors(escapeSrtUserText(text)) by composition", () => {
    // Pin the composition contract — if a future refactor reorders or
    // skips a step inside processSrtUserText, this test fails.
    const samples = [
      "Plain text only",
      '<font color="#00FF00">Green</font>',
      'Text with {literal-braces} and a <font color="#FFFF00">tag</font>',
      "Backslash \\ + brace { + close }",
    ];
    for (const s of samples) {
      expect(processSrtUserText(s)).toBe(preprocessSrtColors(escapeSrtUserText(s)));
    }
  });

  it("is idempotent on text with no user braces and no color tags", () => {
    const plain = "Hello world, no markup at all.";
    expect(processSrtUserText(plain)).toBe(plain);
  });
});

// ── buildAssDocument ─────────────────────────────────────

describe("buildAssDocument", () => {
  it("produces valid ASS with Script Info and V4+ Styles", () => {
    const entries = [{ start: 1000, end: 5000, text: "Hello World" }];
    const result = buildAssDocument(entries);
    expect(result).toContain("[Script Info]");
    expect(result).toContain("[V4+ Styles]");
    expect(result).toContain("[Events]");
    expect(result).toContain("Hello World");
  });

  it("formats timestamps correctly", () => {
    const entries = [
      { start: 3661000, end: 3665000, text: "Test" }, // 1:01:01.000
    ];
    const result = buildAssDocument(entries);
    // ASS timestamp: H:MM:SS.CC (centiseconds)
    expect(result).toContain("1:01:01.00");
  });

  it("applies custom style configuration", () => {
    const entries = [{ start: 0, end: 1000, text: "Styled" }];
    const customStyle = {
      ...DEFAULT_STYLE,
      fontName: "Noto Sans CJK",
      fontSize: 36,
    };
    const result = buildAssDocument(entries, customStyle);
    expect(result).toContain("Noto Sans CJK");
    // Anchor the font-size assertion to the Style line specifically — a bare
    // `,36,` match could also land on the Alignment or Margin fields if any
    // future default ever equals 36, producing a false green.
    expect(result).toMatch(/Style:\s*Default,Noto Sans CJK,36,/);
  });

  it("handles multiple entries", () => {
    const entries = [
      { start: 1000, end: 3000, text: "First" },
      { start: 4000, end: 6000, text: "Second" },
      { start: 7000, end: 9000, text: "Third" },
    ];
    const result = buildAssDocument(entries);
    expect(result).toContain("First");
    expect(result).toContain("Second");
    expect(result).toContain("Third");
  });

  it("handles empty entries array", () => {
    const result = buildAssDocument([]);
    expect(result).toContain("[Script Info]");
    expect(result).toContain("[Events]");
    // No Dialogue lines
    expect(result).not.toContain("Dialogue:");
  });
});

// ── Format detection helpers ─────────────────────────────

describe("isNativeAss", () => {
  it("recognizes .ass files", () => {
    expect(isNativeAss("subtitle.ass")).toBe(true);
  });

  it("recognizes .ssa files", () => {
    expect(isNativeAss("subtitle.ssa")).toBe(true);
  });

  it("rejects .srt files", () => {
    expect(isNativeAss("subtitle.srt")).toBe(false);
  });

  it("is case-insensitive", () => {
    expect(isNativeAss("subtitle.ASS")).toBe(true);
  });
});

describe("isConvertible", () => {
  it("accepts .srt files", () => {
    expect(isConvertible("subtitle.srt")).toBe(true);
  });

  it("accepts .sub files", () => {
    expect(isConvertible("subtitle.sub")).toBe(true);
  });

  it("rejects .ass files (native, not convertible)", () => {
    expect(isConvertible("subtitle.ass")).toBe(false);
  });

  it("rejects .vtt files", () => {
    expect(isConvertible("subtitle.vtt")).toBe(false);
  });
});

// ── SRT pipeline integration — three category-separated suites ──────
// Split into regression / security / composed so a future failure
// vitest line points directly at which property broke (the previous
// single-describe layout meant the failing test name carried the
// category in its own title; with 3 describes the file outline shows
// the three properties as siblings).

// Regression: buildAssDocument must NOT re-escape preprocessSrtColors'
// injected color tags. Round 3 caught a silent break of SRT→HDR color
// conversion when the escape pass touched our trusted overrides.
describe("SRT pipeline integration — color-tag preservation regression", () => {
  it("preserves preprocessSrtColors' injected color tags through buildAssDocument", () => {
    const srt = '<font color="#FF0000">Red text</font>';
    const escaped = escapeSrtUserText(srt);
    const withColors = preprocessSrtColors(escaped);
    const doc = buildAssDocument([{ start: 0, end: 1000, text: withColors }]);
    // The injected color tag must survive untouched — NOT escaped as
    // `\{\\1c&H...` literal text. A passing test means SRT→HDR still
    // carries user colors into the HDR color stage.
    expect(doc).toMatch(/\{\\1c&H0000FF&\}/);
    expect(doc).toContain("Red text");
    // `{` must NOT appear escaped in the Dialogue line emitted for our tag
    expect(doc).not.toMatch(/\\\{\\\\1c/);
  });
});

// Security: a crafted SRT carrying literal `{...}` must NOT be able to
// smuggle ASS overrides into the rendered output. escapeSrtUserText
// neutralizes user braces; the integration here verifies the escaped
// form survives buildAssDocument as literal text.
describe("SRT pipeline integration — override-injection security", () => {
  it("escapes user braces so a crafted SRT cannot smuggle ASS overrides", () => {
    const srt = "{\\an8}hello"; // user-supplied literal
    const escaped = escapeSrtUserText(srt);
    const doc = buildAssDocument([{ start: 0, end: 1000, text: escaped }]);
    // The emitted Dialogue must carry escaped braces — libass renders as
    // literal `{\an8}hello`, NOT as a repositioning override.
    expect(doc).toContain("\\{\\\\an8\\}hello");
  });
});

// Composed: both properties hold simultaneously when user braces and
// our injected color tags are present in the same input.
describe("SRT pipeline integration — composed (braces + color tags)", () => {
  it("handles a composed SRT with both user braces and color tags correctly", () => {
    const srt = '{literal}<font color="#00FF00">green</font>{more}';
    const escaped = escapeSrtUserText(srt);
    const withColors = preprocessSrtColors(escaped);
    const doc = buildAssDocument([{ start: 0, end: 1000, text: withColors }]);
    // User braces survive as literal text
    expect(doc).toContain("\\{literal\\}");
    expect(doc).toContain("\\{more\\}");
    // Our injected color override survives unescaped (BGR order)
    expect(doc).toMatch(/\{\\1c&H00FF00&\}/);
    expect(doc).toContain("green");
  });
});

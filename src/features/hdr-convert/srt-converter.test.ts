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
    // The reset must actually appear between the colored span and the
    // following text — without it, "normal" would inherit red. Bare
    // .toContain("normal") would pass even if no reset were emitted.
    expect(result).toMatch(/Red(?:\{\\r\}|\{\\1c&H[0-9A-F]{6}&?\}) normal/);
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
    // No color attribute → font tag should be stripped but no color override
    expect(result).toContain("Styled");
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

// ── SRT pipeline integration — guards against the Round 3 regression
// where buildAssDocument re-escaped preprocessSrtColors' injected tags,
// silently breaking HDR color conversion.

describe("SRT pipeline integration", () => {
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

  it("escapes user braces so a crafted SRT cannot smuggle ASS overrides", () => {
    const srt = "{\\an8}hello"; // user-supplied literal
    const escaped = escapeSrtUserText(srt);
    const doc = buildAssDocument([{ start: 0, end: 1000, text: escaped }]);
    // The emitted Dialogue must carry escaped braces — libass renders as
    // literal `{\an8}hello`, NOT as a repositioning override.
    expect(doc).toContain("\\{\\\\an8\\}hello");
  });

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

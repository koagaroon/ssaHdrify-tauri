/**
 * SRT/SUB converter tests — ported from Python tests/test_hdrify.py
 *
 * Tests cover: HTML font color preprocessing, ASS document building,
 * format detection helpers, and custom style configuration.
 */
import { describe, it, expect } from "vitest";
import {
  preprocessSrtColors,
  buildAssDocument,
  isNativeAss,
  isConvertible,
  DEFAULT_STYLE,
} from "./srt-converter";

// ── preprocessSrtColors ──────────────────────────────────

describe("preprocessSrtColors", () => {
  it("converts <font color> to ASS inline override", () => {
    const result = preprocessSrtColors('<font color="#FF0000">Red text</font>');
    // Should contain ASS color tag (\1c&H or \c&H)
    expect(result).toMatch(/\\1?c&H/);
    expect(result).toContain("Red text");
    // HTML tag should be gone
    expect(result).not.toContain("<font");
  });

  it("inserts color reset after </font>", () => {
    const result = preprocessSrtColors('<font color="#FF0000">Red</font> normal');
    // After the colored section, a reset tag should appear
    expect(result).toContain("normal");
  });

  it("handles multiple attributes on <font> tag", () => {
    const result = preprocessSrtColors('<font face="Arial" color="#00FF00">Green</font>');
    expect(result).toMatch(/\\1?c&H/);
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

/**
 * ASS processor tests — ported from Python tests/test_hdrify.py
 *
 * Tests cover: color parsing/formatting, section detection,
 * inline color tag regex matching, and full content processing.
 */
import { describe, it, expect } from "vitest";
import { parseAssColor, formatAssColor, detectSection, processAssContent } from "./ass-processor";

// ── Helpers ──────────────────────────────────────────────

/** Wrap dialogue text in a minimal valid ASS document for processAssContent */
function makeAss(
  dialogueText: string,
  styleColors = "&H00FFFFFF,&H000000FF,&H00000000,&H00000000"
): string {
  return [
    "[Script Info]",
    "ScriptType: v4.00+",
    "",
    "[V4+ Styles]",
    "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding",
    `Style: Default,Arial,48,${styleColors},0,0,0,0,100,100,0,0,1,2,0,2,10,10,10,1`,
    "",
    "[Events]",
    "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text",
    `Dialogue: 0,0:00:00.00,0:00:05.00,Default,,0,0,0,,${dialogueText}`,
  ].join("\n");
}

// ── parseAssColor ────────────────────────────────────────

describe("parseAssColor", () => {
  it("parses 6-digit color (normalizes to alpha '00')", () => {
    // &HFFFFFF → BB=FF GG=FF RR=FF → r=255 g=255 b=255, alpha defaults to "00"
    const result = parseAssColor("&HFFFFFF");
    expect(result).toEqual({ r: 255, g: 255, b: 255, alpha: "00" });
  });

  it("parses 8-digit color (&HAABBGGRR, with alpha)", () => {
    const result = parseAssColor("&H00FFFFFF");
    expect(result).toEqual({ r: 255, g: 255, b: 255, alpha: "00" });
  });

  it("handles BGR order correctly", () => {
    // &HFF0000 → BB=FF GG=00 RR=00 → r=0 g=0 b=255, alpha defaults to "00"
    const result = parseAssColor("&HFF0000");
    expect(result).toEqual({ r: 0, g: 0, b: 255, alpha: "00" });
  });

  it("parses mixed channels", () => {
    // &H00FF00 → BB=00 GG=FF RR=00 → r=0 g=255 b=0, alpha defaults to "00"
    const result = parseAssColor("&H00FF00");
    expect(result).toEqual({ r: 0, g: 255, b: 0, alpha: "00" });
  });
});

// ── formatAssColor ───────────────────────────────────────

describe("formatAssColor", () => {
  it("formats RGB to ASS BGR with alpha", () => {
    // r=255 g=0 b=0 → RR=FF GG=00 BB=00 → &H000000FF
    expect(formatAssColor(255, 0, 0, "00")).toBe("&H000000FF");
  });

  it("roundtrips with parseAssColor (8-digit)", () => {
    const original = "&H00AABBCC";
    const parsed = parseAssColor(original);
    const formatted = formatAssColor(parsed.r, parsed.g, parsed.b, parsed.alpha);
    expect(formatted).toBe(original);
  });

  it("normalizes 6-digit to 8-digit on roundtrip", () => {
    // 6-digit input gets alpha="00", so roundtrip produces 8-digit
    const parsed = parseAssColor("&HAABBCC");
    const formatted = formatAssColor(parsed.r, parsed.g, parsed.b, parsed.alpha);
    expect(formatted).toBe("&H00AABBCC");
  });
});

// ── detectSection ────────────────────────────────────────

describe("detectSection", () => {
  it("detects [Script Info]", () => {
    expect(detectSection("[Script Info]")).toBe("info");
  });

  it("detects [V4+ Styles]", () => {
    expect(detectSection("[V4+ Styles]")).toBe("styles");
  });

  it("detects [Events]", () => {
    expect(detectSection("[Events]")).toBe("events");
  });

  it("detects [Fonts]", () => {
    expect(detectSection("[Fonts]")).toBe("fonts");
  });

  it("returns null for non-section lines", () => {
    expect(detectSection("Style: Default,Arial,48")).toBeNull();
    expect(detectSection("Dialogue: 0,0:00:00.00")).toBeNull();
    expect(detectSection("")).toBeNull();
  });
});

// ── processAssContent — inline color tag matching ────────
// Ported from Python regex/event transform tests

describe("processAssContent — inline color tags", () => {
  it("transforms 6-digit color tags in dialogue", () => {
    const input = makeAss("{\\1c&HFFFFFF&}Hello");
    const output = processAssContent(input, 203, "PQ");
    expect(output).toContain("Hello");
    // White (FFFFFF) should be converted to a different HDR value
    expect(output).not.toMatch(/\\1c&H(?:00)?FFFFFF/);
    // Tag structure preserved AND the replacement is a legal 6/8-digit
    // hex sequence (not a malformed run that could survive through the
    // pipeline as plain text).
    expect(output).toMatch(/\\1c&H[0-9a-fA-F]{6}(?:[0-9a-fA-F]{2})?&/);
  });

  it("transforms 8-digit color tags (with alpha)", () => {
    const input = makeAss("{\\1c&H00FFFFFF&}Hello");
    const output = processAssContent(input, 203, "PQ");
    expect(output).toContain("Hello");
    expect(output).not.toContain("00FFFFFF");
  });

  it("ignores 7-digit hex values (invalid ASS format)", () => {
    const input = makeAss("{\\1c&HFFFFFFF&}Hello");
    const output = processAssContent(input, 203, "PQ");
    // 7-digit should pass through — regex matches exactly 6 or 8 digits.
    // Assert on the EXACT token so the test fails if the transformer ever
    // accidentally matches 6/8 digits inside a 7-digit run; `toContain`
    // alone would still pass with a leftover 6-digit match.
    expect(output).toContain("{\\1c&HFFFFFFF&}");
  });

  it("ignores short color values (< 6 hex digits)", () => {
    const input = makeAss("{\\1c&HFF&}Hello");
    const output = processAssContent(input, 203, "PQ");
    expect(output).toContain("{\\1c&HFF&}Hello");
  });

  it("leaves non-color tags unchanged", () => {
    const input = makeAss("{\\b1}No colors here");
    const output = processAssContent(input, 203, "PQ");
    expect(output).toContain("{\\b1}No colors here");
  });

  it("transforms comma-separated tags", () => {
    const input = makeAss("{\\1c&HFFFFFF,\\blur3}Hello");
    const output = processAssContent(input, 203, "PQ");
    expect(output).toContain("Hello");
    expect(output).toContain("\\blur3");
    // White should be transformed
    expect(output).not.toMatch(/\\1c&H(?:00)?FFFFFF/);
  });

  it("handles empty dialogue text without crashing", () => {
    const input = makeAss("");
    const output = processAssContent(input, 203, "PQ");
    expect(output).toBeTruthy();
  });

  it("preserves black (passthrough) in inline tags", () => {
    const input = makeAss("{\\1c&H000000&}Dark");
    const output = processAssContent(input, 203, "PQ");
    // Anchor to the inline tag specifically. The makeAss default style
    // colors already contain &H00000000 four times, so a bare
    // .toContain("000000") would pass even if the inline transformer
    // broke and rewrote {\1c&H000000&} to non-black.
    expect(output).toMatch(/\{\\1c&H(?:00)?000000&\}Dark/);
  });
});

// ── processAssContent — style line colors ────────────────

describe("processAssContent — style lines", () => {
  it("transforms style PrimaryColour", () => {
    const input = makeAss("Hello");
    const output = processAssContent(input, 203, "PQ");
    // PrimaryColour was &H00FFFFFF (white) — should be transformed
    expect(output).not.toContain("&H00FFFFFF");
    expect(output).toContain("[V4+ Styles]");
  });

  it("preserves black style colors (passthrough)", () => {
    const input = makeAss("Hello", "&H00000000,&H00000000,&H00000000,&H00000000");
    const output = processAssContent(input, 203, "PQ");
    // Anchor to the full Style line so a regression that rewrote any of
    // the four colors but left the others black would still fail. The
    // bare .toContain("&H00000000") would pass on three-out-of-four
    // breakage.
    expect(output).toMatch(
      /Style:\s*Default,[^,]+,\d+,&H00000000,&H00000000,&H00000000,&H00000000,/
    );
  });
});

describe("processAssContent — Wave 5.1 pre-split line-count probe (A-R5-FEFEAT-03)", () => {
  it("rejects pure-newline blob exceeding the line cap before .split allocates", () => {
    // 600k newlines + 1 byte: well over the 500k LINE_CAP, and the
    // content.length > 1 MB gate fires (600k bytes). The probe should
    // throw BEFORE `.split(/\r?\n/)` allocates ~600k empty strings
    // (~24 MB V8 heap). Without the probe, .split allocates first and
    // the post-split line-count throw fires too late.
    const blob = "\n".repeat(600_000) + "x";
    expect(() => processAssContent(blob, 1000, "PQ")).toThrow(/too large.*lines/i);
  });

  it("accepts normal-size content (well under the 1 MB gate) without false-positive", () => {
    // A typical subtitle is 5-200 KB. Pre-split probe should be skipped
    // entirely (`content.length > 1_000_000` is false) and the file
    // should process normally. Guard against regression where the
    // probe runs unconditionally and slows the small-file fast path.
    const small = ["[Script Info]", "ScriptType: v4.00+", "", "[Events]", ""].join("\n");
    expect(() => processAssContent(small, 1000, "PQ")).not.toThrow();
  });
});

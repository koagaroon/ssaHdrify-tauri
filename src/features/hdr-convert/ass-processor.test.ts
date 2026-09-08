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
    // Pin the exact transformed value rather than a 6/8-digit
    // structural match. A regression producing wrong-but-still-6-digit
    // hex would have silently passed; the structural match was too
    // loose. White at 203 nits
    // PQ is the reference BT.2408 white point — Color.js +
    // sRgbToHdr's PQ path emit a deterministic value for it
    // (&H949494& at the time of this pin; if Color.js or sRgbToHdr's
    // math changes intentionally, update the expected value here
    // along with the corresponding test in color-engine.test.ts).
    expect(output).toContain("{\\1c&H949494&}Hello");
  });

  it("transforms 8-digit color tags (with alpha)", () => {
    const input = makeAss("{\\1c&H00FFFFFF&}Hello");
    const output = processAssContent(input, 203, "PQ");
    expect(output).toContain("Hello");
    expect(output).not.toContain("00FFFFFF");
  });

  it("leaves the unsupported seven-digit inline alpha spelling unchanged", () => {
    const input = makeAss("{\\1c&HFFFFFFF&}Hello");
    const output = processAssContent(input, 203, "PQ");
    // Keep this unsupported inline alpha spelling intact, without converting a prefix.
    // Assert on the EXACT token so the test fails if the transformer ever
    // accidentally matches 6/8 digits inside a 7-digit run; `toContain`
    // alone would still pass with a leftover 6-digit match.
    expect(output).toContain("{\\1c&HFFFFFFF&}");
  });

  it("transforms short color values without requiring leading zeros", () => {
    const input = makeAss("{\\1c&HFF&}Hello");
    const output = processAssContent(input, 203, "PQ");
    expect(output).toContain("{\\1c&H385388&}Hello");
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

describe("processAssContent — pre-split line-count probe", () => {
  it("rejects pure-newline blob exceeding the line cap before .split allocates", () => {
    // Fixture must EXCEED the > 1_000_000 byte probe gate; a fixture
    // with 600k newlines (600k bytes) falls BELOW the gate, so the
    // probe code path is never actually exercised and the test would
    // pass via the post-split line-count throw at the end of
    // processAssContent. 1.1 MB of pure newlines lands above the gate
    // AND above LINE_CAP, so the probe path is the actual code path
    // the test pins.
    const blob = "\n".repeat(1_100_000) + "x";
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

describe("equivalent color representations", () => {
  it.each(["0", "F", "FF", "F00", "FF00", "1FF00"])(
    "converts inline %s exactly like its six-digit form",
    (hex) => {
      expect(processAssContent(makeAss(`{\\c&H${hex}&}text`))).toBe(
        processAssContent(makeAss(`{\\c&H${hex.padStart(6, "0")}&}text`))
      );
    }
  );

  it.each(["0", "F", "FF", "F00", "FF00", "1FF00", "FFF0000"])(
    "converts style hex %s exactly like its eight-digit form",
    (hex) => {
      const colors = `&h${hex},&H0,&H0,&H0`;
      const canonical = `&H${hex.padStart(8, "0")},&H00000000,&H00000000,&H00000000`;
      expect(processAssContent(makeAss("text", colors))).toBe(
        processAssContent(makeAss("text", canonical))
      );
    }
  );

  function makeSsa(color: string): string {
    return [
      "[Script Info]",
      "ScriptType: v4.00",
      "[V4 Styles]",
      "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, TertiaryColour, BackColour, Bold, Italic, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, AlphaLevel, Encoding",
      `Style: Default,Arial,20,${color},${color},0,${color},0,0,1,2,0,2,10,10,10,0,1`,
      "[Events]",
      "Format: Marked, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text",
      "Dialogue: Marked=0,0:00:01.00,0:00:02.00,Default,,0,0,0,,text",
    ].join("\n");
  }

  it.each([0, 255, 16777215, -2147483648, 4294967295])(
    "converts SSA integer color %s with its 32-bit alpha intact",
    (color) => {
      const expected = makeSsa(`&H${(color >>> 0).toString(16).padStart(8, "0")}`);
      expect(processAssContent(makeSsa(String(color)))).toBe(processAssContent(expected));
    }
  );

  it.each(["4294967296", "-2147483649", "99999999999", "12x", "&H123456789"])(
    "preserves out-of-range or malformed style color %s",
    (color) => {
      expect(processAssContent(makeSsa(color))).toBe(makeSsa(color));
    }
  );

  it("does not convert prefixes of overlong or malformed inline colors", () => {
    for (const hex of ["123456789", "FFFFFG", ""]) {
      const tag = `{\\c&H${hex}&}text`;
      expect(processAssContent(makeAss(tag))).toContain(tag);
    }
  });
});

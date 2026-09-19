/**
 * ASS processor tests — ported from Python tests/test_hdrify.py
 *
 * Tests cover: color parsing/formatting, section detection,
 * inline color tag regex matching, and full content processing.
 */
import { describe, it, expect } from "vitest";
import { parseAssColor, formatAssColor, detectSection, processAssContent } from "./ass-processor";
import { runChain } from "../chain/chain-runtime";
import type { ChainStep } from "../chain/chain-types";
import { parseSubtitle } from "../../lib/subtitle-parser";

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
  it("changes only real override blocks, preserving literal and escaped tag-shaped text", () => {
    const text = String.raw`literal \c&HFFFFFF& \{\c&HFFFFFF&\} \\{\c&HFFFFFF&\} {\t(0,500,\c&HFFFFFF&)}real {unclosed \c&HFFFFFF&`;
    const expected = String.raw`literal \c&HFFFFFF& \{\c&HFFFFFF&\} \\{\c&HFFFFFF&\} {\t(0,500,\c&H949494&)}real {unclosed \c&HFFFFFF&`;
    expect(processAssContent(makeAss(text), 203, "PQ")).toContain(expected);
  });

  it("leaves unsupported numbered color tags unchanged inside real blocks", () => {
    const text = String.raw`{\0c&HFFFFFF&\5c&HFFFFFF&\9c&HFFFFFF&\4c&HFFFFFF&}text`;
    const expected = String.raw`{\0c&HFFFFFF&\5c&HFFFFFF&\9c&HFFFFFF&\4c&H949494&}text`;
    expect(processAssContent(makeAss(text), 203, "PQ")).toContain(expected);
  });

  it("uses the declared final Text field without changing metadata or comments", () => {
    const input = [
      "[Events]",
      "Format: End, Name, Start, Effect, Text",
      String.raw`Dialogue: 0:00:02.00,{\c&HFFFFFF&},0:00:01.00,\c&HFFFFFF&,{\c&HFFFFFF&}hello, world`,
      String.raw`Comment: 0:00:02.00,{\c&HFFFFFF&},0:00:01.00,,{\c&HFFFFFF&}comment`,
      "[Unknown]",
      String.raw`Dialogue: {\c&HFFFFFF&}`,
    ].join("\n");
    const expected = input.replace(
      String.raw`,{\c&HFFFFFF&}hello, world`,
      String.raw`,{\c&H949494&}hello, world`
    );
    expect(processAssContent(input, 203, "PQ")).toBe(expected);
  });

  it("uses ten event columns when no Format is declared", () => {
    const input = String.raw`[Events]
Dialogue: 0,0:00:01.00,0:00:02.00,Default,{\c&HFFFFFF&},0,0,0,\c&HFFFFFF&,{\c&HFFFFFF&}text`;
    expect(processAssContent(input, 203, "PQ")).toBe(
      input.replace(String.raw`,{\c&HFFFFFF&}text`, String.raw`,{\c&H949494&}text`)
    );
  });

  it.each(["Layer, Start, End", "Text, Start, End", "Text, Text"])(
    "rejects an ambiguous declared Text boundary: %s",
    (format) => {
      expect(() => processAssContent(`[Events]\nFormat: ${format}`)).toThrow(/one final Text/);
    }
  );

  it("bounds declared event fields before accepting their Text boundary", () => {
    expect(() => processAssContent(`[Events]\nFormat: ${"Other,".repeat(1023)}Text`)).not.toThrow();
    expect(() => processAssContent(`[Events]\nFormat: ${"Other,".repeat(1024)}Text`)).toThrow(
      /too many fields/
    );
  });

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

describe("processAssContent — physical lines", () => {
  it.each(["\n", "\r\n", "\r"])("converts colors while preserving %j line endings", (ending) => {
    const input =
      makeAss("{\\c&HFFFFFF&}text", "&H00FFFFFF,&H00000000,&H00000000,&H00000000").replace(
        /\n/g,
        ending
      ) + ending;
    const output = processAssContent(input, 203, "PQ");
    expect(output).toBe(input.replace(/FFFFFF/g, "949494"));
    expect(output.match(/\r\n|\r|\n/g)).toEqual(input.match(/\r\n|\r|\n/g));
  });

  it("preserves mixed endings, blank lines, and a missing final newline", () => {
    const endings = ["\r", "\r\n", "\n"];
    let index = 0;
    const input = makeAss(
      "{\\c&HFFFFFF&}text",
      "&H00FFFFFF,&H00000000,&H00000000,&H00000000"
    ).replace(/\n/g, () => endings[index++ % endings.length]!);
    expect(processAssContent(input, 203, "PQ")).toBe(input.replace(/FFFFFF/g, "949494"));
  });

  it("preserves CR lines and literal text through either HDR/shift chain order", () => {
    const input = makeAss(String.raw`literal \c&HFFFFFF& {\c&HFFFFFF&}colored`).replace(
      /\n/g,
      "\r"
    );
    const hdr: ChainStep = { kind: "hdr", params: { eotf: "PQ", brightness: 203 } };
    const shift: ChainStep = { kind: "shift", params: { offsetMs: 1000 } };
    for (const steps of [
      [hdr, shift],
      [shift, hdr],
    ]) {
      const result = runChain({
        inputPath: "C:\\subs\\example.ass",
        content: input,
        plan: { steps, outputTemplate: "{name}.chain{ext}" },
      });
      expect(result.content).toContain(String.raw`literal \c&HFFFFFF& {\c&H949494&}colored`);
      expect(result.content.match(/\r\n|\r|\n/g)).toEqual(input.match(/\r\n|\r|\n/g));
      expect(parseSubtitle(result.content).captions[0]).toMatchObject({ start: 1000, end: 6000 });
    }
  });

  it("rejects content exceeding the line cap before accumulating output", () => {
    const blob = "\n".repeat(1_100_000) + "x";
    expect(() => processAssContent(blob, 1000, "PQ")).toThrow(/too large.*lines/i);
  });

  it("accepts ordinary content", () => {
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

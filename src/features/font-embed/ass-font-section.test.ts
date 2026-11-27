import { describe, expect, it } from "vitest";
import { assertAssShape, insertFontsSection } from "./ass-font-section";

describe("ass-font-section", () => {
  it("accepts UTF-8 BOM before [Script Info] and preserves it on rewrite", () => {
    const bomAss =
      "\uFEFF[Script Info]\r\nTitle: Demo\r\n\r\n[Events]\r\nDialogue: 0,0:00:00.00,0:00:01.00,Default,Hi\r\n";
    expect(() => assertAssShape(bomAss)).not.toThrow();
    const out = insertFontsSection(bomAss, "[Fonts]\nfontname: arial.ttf\nABCD\n");
    expect(out.startsWith("\uFEFF[Script Info]\r\n")).toBe(true);
    expect(out).toContain("\r\n\r\n[Fonts]\r\nfontname: arial.ttf\r\nABCD\r\n\r\n[Events]\r\n");
  });

  it("does not prematurely terminate existing [Fonts] replacement on uppercase UUEncode bracket lines", () => {
    const existing = [
      "[Script Info]",
      "Title: UUEncode Bracket Tail",
      "",
      "[Fonts]",
      "fontname: old.ttf",
      "[A]",
      "[ABCD12+]",
      "TRAILING_OLD_PAYLOAD",
      "",
      "[EVENTS]",
      "Dialogue: 0,0:00:00.00,0:00:01.00,Default,Kept",
    ].join("\n");

    const replaced = insertFontsSection(existing, "[Fonts]\nfontname: new.ttf\nWXYZ\n");
    expect(replaced).toContain("fontname: new.ttf");
    expect(replaced).not.toContain("old.ttf");
    expect(replaced).not.toContain("[ABCD12+]");
    expect(replaced).not.toContain("TRAILING_OLD_PAYLOAD");
    expect(replaced).toContain("[EVENTS]\nDialogue: 0,0:00:00.00,0:00:01.00,Default,Kept");
  });

  it("preserves U+2028 and U+2029 inside untouched [Events] dialogue lines", () => {
    const ass = `[Script Info]\nTitle: LineSep\n\n[Events]\nDialogue: 0,0:00:00.00,0:00:01.00,Default,A\u2028B\u2029C\n`;
    const out = insertFontsSection(ass, "[Fonts]\nfontname: arial.ttf\nABCD\n");
    expect(out).toContain("Dialogue: 0,0:00:00.00,0:00:01.00,Default,A\u2028B\u2029C\n");
  });
});

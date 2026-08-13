import { describe, expect, it } from "vitest";

import {
  filterAndDedupeStyleEditPaths,
  isStyleEditWriteDisabled,
  reconcileStyleSelection,
  validateStyleEditOperations,
  validateStyleFontFamily,
} from "./style-ui-state";

describe("Style Edit UI state", () => {
  it("filters unsupported inputs and keeps only the first duplicate subtitle path", () => {
    expect(
      filterAndDedupeStyleEditPaths([
        "C:\\subs\\episode.ass",
        "C:\\subs\\episode.ass",
        "C:\\subs\\notes.txt",
        "C:\\subs\\legacy.SSA",
      ])
    ).toEqual(["C:\\subs\\episode.ass", "C:\\subs\\legacy.SSA"]);
  });

  it("selects new changeable rows without restoring a manual deselection", () => {
    expect(
      reconcileStyleSelection(
        new Set(["kept"]),
        new Set(["kept", "cleared"]),
        new Set(["kept", "cleared", "new"])
      )
    ).toEqual(new Set(["kept", "new"]));
  });
  it("requires at least one independently enabled operation", () => {
    const result = validateStyleEditOperations({
      fontFamilyEnabled: false,
      targetFontFamily: "Arial",
      sourceFilterEnabled: false,
      sourceFontFamily: "",
      fontSizeEnabled: false,
      targetFontSize: "48",
    });

    expect(result.hasEnabledOperation).toBe(false);
    expect(result.valid).toBe(false);
  });

  it("accepts font-family and font-size operations independently", () => {
    const fontOnly = validateStyleEditOperations({
      fontFamilyEnabled: true,
      targetFontFamily: "Microsoft YaHei",
      sourceFilterEnabled: false,
      sourceFontFamily: "",
      fontSizeEnabled: false,
      targetFontSize: "",
    });
    expect(fontOnly.valid).toBe(true);
    expect(fontOnly.targetFontFamily).toBe("Microsoft YaHei");
    expect(fontOnly.targetFontSize).toBeNull();

    const sizeOnly = validateStyleEditOperations({
      fontFamilyEnabled: false,
      targetFontFamily: "",
      sourceFilterEnabled: false,
      sourceFontFamily: "",
      fontSizeEnabled: true,
      targetFontSize: "48.5",
    });
    expect(sizeOnly.valid).toBe(true);
    expect(sizeOnly.targetFontFamily).toBeNull();
    expect(sizeOnly.targetFontSize).toBe(48.5);
  });

  it("validates the optional source-family filter only when enabled", () => {
    const withoutFilter = validateStyleEditOperations({
      fontFamilyEnabled: true,
      targetFontFamily: "Arial",
      sourceFilterEnabled: false,
      sourceFontFamily: "",
      fontSizeEnabled: false,
      targetFontSize: "",
    });
    expect(withoutFilter.valid).toBe(true);

    const withBlankFilter = validateStyleEditOperations({
      fontFamilyEnabled: true,
      targetFontFamily: "Arial",
      sourceFilterEnabled: true,
      sourceFontFamily: "",
      fontSizeEnabled: false,
      targetFontSize: "",
    });
    expect(withBlankFilter.sourceFontError).toBe("required");
    expect(withBlankFilter.valid).toBe(false);
  });

  it("rejects family values that would break or spoof a Style row", () => {
    expect(validateStyleFontFamily(" ")).toBe("required");
    expect(validateStyleFontFamily(" Arial")).toBe("surrounding_whitespace");
    expect(validateStyleFontFamily("Arial,Italic")).toBe("comma");
    expect(validateStyleFontFamily("Arial\nFake")).toBe("control");
    expect(validateStyleFontFamily(`Arial\u202eFake`)).toBe("control");
    expect(validateStyleFontFamily("字".repeat(129))).toBe("too_long");
  });

  it("rejects malformed and out-of-range font sizes", () => {
    for (const targetFontSize of ["", "12abc", "Infinity", "0", "201"]) {
      const result = validateStyleEditOperations({
        fontFamilyEnabled: false,
        targetFontFamily: "",
        sourceFilterEnabled: false,
        sourceFontFamily: "",
        fontSizeEnabled: true,
        targetFontSize,
      });
      expect(result.fontSizeInvalid, targetFontSize).toBe(true);
      expect(result.valid, targetFontSize).toBe(false);
    }
  });

  it("enables Write only for a valid effective selected change", () => {
    expect(
      isStyleEditWriteDisabled({
        fileCount: 1,
        busy: false,
        operationsValid: true,
        effectiveSelectedRowCount: 1,
      })
    ).toBe(false);

    expect(
      isStyleEditWriteDisabled({
        fileCount: 1,
        busy: false,
        operationsValid: true,
        effectiveSelectedRowCount: 0,
      })
    ).toBe(true);
    expect(
      isStyleEditWriteDisabled({
        fileCount: 1,
        busy: false,
        operationsValid: false,
        effectiveSelectedRowCount: 1,
      })
    ).toBe(true);
  });
});

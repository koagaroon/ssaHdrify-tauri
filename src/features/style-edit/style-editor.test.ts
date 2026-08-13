import { describe, expect, it } from "vitest";

import {
  StyleEditError,
  applyStyleEdit,
  inspectStyleDocument,
  planStyleEdit,
  type StyleEditOperations,
} from "./style-editor";

const EDIT_BOTH: StyleEditOperations = {
  fontFamily: { enabled: true, targetFamily: "Microsoft YaHei" },
  fontSize: { enabled: true, targetSize: 52 },
};

function assDocument(
  styleLines: string[],
  options?: { format?: string; legacy?: boolean }
): string {
  const section = options?.legacy ? "[V4 Styles]" : "[V4+ Styles]";
  const format = options?.format ?? "Name, Fontname, Fontsize, Bold, Italic";
  return [
    "[Script Info]",
    "Title: Style edit test",
    "",
    section,
    `Format: ${format}`,
    ...styleLines,
    "",
    "[Events]",
    "Format: Layer, Start, End, Style, Text",
    "Comment: 0,0:00:00.00,0:00:01.00,Default,Arial,48 must stay",
    "Dialogue: 0,0:00:00.00,0:00:01.00,Default,{\\fnArial\\fs48}Hello, world",
  ].join("\n");
}

describe("planStyleEdit and applyStyleEdit", () => {
  it("uses the declared Format order and preserves every unrelated character", () => {
    const input = assDocument(
      ["Style: Default, 48 , Arial ,0,0", "Style: Alt, 36 , Noto Sans CJK SC ,0,0"],
      { format: "Name, Fontsize, Fontname, Bold, Italic" }
    ).replace(/\n/g, "\r\n");

    const plan = planStyleEdit(input, EDIT_BOTH);
    expect(plan).toMatchObject({
      styleCount: 2,
      changedStyleCount: 2,
      changedFieldCount: 4,
      unchangedStyleCount: 0,
    });
    expect(plan.rows[0]).toMatchObject({
      styleName: "Default",
      fontFamilyBefore: "Arial",
      fontFamilyAfter: "Microsoft YaHei",
      fontSizeBefore: "48",
      fontSizeAfter: "52",
      changes: ["fontFamily", "fontSize"],
    });

    const result = applyStyleEdit(input, EDIT_BOTH);
    expect(result.content).toContain("Style: Default, 52 , Microsoft YaHei ,0,0\r\n");
    expect(result.content).toContain("Style: Alt, 52 , Microsoft YaHei ,0,0\r\n");
    expect(result.content).toContain(
      "Dialogue: 0,0:00:00.00,0:00:01.00,Default,{\\fnArial\\fs48}Hello, world"
    );
    expect(result.content).toContain("Comment: 0,0:00:00.00,0:00:01.00,Default,Arial,48 must stay");
    expect(result.content.match(/\r\n/g)?.length).toBe(input.match(/\r\n/g)?.length);
    expect(result.content.replace(/^.*Style:.*$/gm, "")).toBe(input.replace(/^.*Style:.*$/gm, ""));
  });

  it("supports legacy SSA [V4 Styles] records", () => {
    const input = assDocument(["Style: Default,Arial,20,0,0"], { legacy: true });
    const result = applyStyleEdit(input, EDIT_BOTH);
    expect(result.plan.rows[0]!.section).toBe("ssa-v4");
    expect(result.content).toContain("Style: Default,Microsoft YaHei,52,0,0");
  });

  it("terminates style parsing at an unknown header longer than 100 characters", () => {
    const input = [
      "[V4+ Styles]",
      "Format: Name, Fontname, Fontsize",
      "Style: Default,Arial,20",
      `[${"x".repeat(101)}]`,
      "Style: This is unrelated text,Arial,20",
    ].join("\n");
    const result = applyStyleEdit(input, EDIT_BOTH);
    expect(result.plan.styleCount).toBe(1);
    expect(result.content).toContain("Style: Default,Microsoft YaHei,52");
    expect(result.content).toContain("Style: This is unrelated text,Arial,20");
  });

  it("terminates style parsing at a malformed bracket-led section line", () => {
    const input = [
      "[V4+ Styles]",
      "Format: Name, Fontname, Fontsize",
      "Style: Default,Arial,20",
      "[Events] trailing text without a final bracket",
      "Style: This is unrelated text,Arial,20",
    ].join("\n");
    const result = applyStyleEdit(input, EDIT_BOTH);
    expect(result.plan.styleCount).toBe(1);
    expect(result.content).toContain("Style: Default,Microsoft YaHei,52");
    expect(result.content).toContain("Style: This is unrelated text,Arial,20");
  });

  it("applies an optional source-family filter case-insensitively after NFC normalization", () => {
    const input = assDocument([
      "Style: Default,Arial,20,0,0",
      "Style: Exact,ARIAL,22,0,0",
      "Style: Accent,Cafe\u0301,23,0,0",
      "Style: Other,Noto Sans,24,0,0",
    ]);
    const operations: StyleEditOperations = {
      fontFamily: {
        enabled: true,
        sourceFamily: "arial",
        targetFamily: "Microsoft YaHei",
      },
    };

    const plan = planStyleEdit(input, operations);
    expect(plan.changedStyleCount).toBe(2);
    expect(plan.rows.map((row) => row.willChange)).toEqual([true, true, false, false]);
    const output = applyStyleEdit(input, operations).content;
    expect(output).toContain("Style: Default,Microsoft YaHei,20,0,0");
    expect(output).toContain("Style: Exact,Microsoft YaHei,22,0,0");
    expect(output).toContain("Style: Accent,Cafe\u0301,23,0,0");
    expect(output).toContain("Style: Other,Noto Sans,24,0,0");

    const accentOutput = applyStyleEdit(input, {
      fontFamily: {
        enabled: true,
        sourceFamily: "Café",
        targetFamily: "Accent Sans",
      },
    }).content;
    expect(accentOutput).toContain("Style: Accent,Accent Sans,23,0,0");
  });

  it("changes every style family when the source-family filter is absent", () => {
    const input = assDocument(["Style: Default,Arial,20,0,0", "Style: Other,Noto Sans,24,0,0"]);
    const output = applyStyleEdit(input, {
      fontFamily: { enabled: true, targetFamily: "Microsoft YaHei" },
    }).content;
    expect(output).toContain("Style: Default,Microsoft YaHei,20,0,0");
    expect(output).toContain("Style: Other,Microsoft YaHei,24,0,0");
  });

  it("applies only explicitly selected row IDs", () => {
    const input = assDocument(["Style: Default,Arial,20,0,0", "Style: Signs,Noto Sans,24,0,0"]);
    const plan = planStyleEdit(input, EDIT_BOTH);
    const selected = [plan.rows[1]!.id];
    const result = applyStyleEdit(input, EDIT_BOTH, selected);

    expect(result.appliedRowIds).toEqual(selected);
    expect(result.changedStyleCount).toBe(1);
    expect(result.changedFieldCount).toBe(2);
    expect(result.content).toContain("Style: Default,Arial,20,0,0");
    expect(result.content).toContain("Style: Signs,Microsoft YaHei,52,0,0");
  });

  it("treats an explicit empty selection as a no-op", () => {
    const input = assDocument(["Style: Default,Arial,20,0,0"]);
    const result = applyStyleEdit(input, EDIT_BOTH, []);
    expect(result.content).toBe(input);
    expect(result.appliedRowIds).toEqual([]);
  });

  it("preserves a UTF-8 BOM, mixed line endings, bare CR, and missing final newline", () => {
    const input =
      "\uFEFF[Script Info]\r\nTitle: Mixed\n[V4+ Styles]\r" +
      "Format: Fontsize, Name, Fontname\r\nStyle: 18,Default,Arial\n" +
      "[Events]\rDialogue: 0,0:00:00.00,0:00:01.00,Default,{\\fs18}Text";
    const output = applyStyleEdit(input, EDIT_BOTH).content;
    expect(output).toBe(
      "\uFEFF[Script Info]\r\nTitle: Mixed\n[V4+ Styles]\r" +
        "Format: Fontsize, Name, Fontname\r\nStyle: 52,Default,Microsoft YaHei\n" +
        "[Events]\rDialogue: 0,0:00:00.00,0:00:01.00,Default,{\\fs18}Text"
    );
  });

  it("does not rewrite a numerically equivalent existing font size", () => {
    const input = assDocument(["Style: Default,Arial,48.0,0,0"]);
    const result = applyStyleEdit(input, {
      fontSize: { enabled: true, targetSize: 48 },
    });
    expect(result.plan.changedStyleCount).toBe(0);
    expect(result.content).toBe(input);
  });

  it("repairs non-decimal JavaScript numeric spellings instead of treating them as equivalent", () => {
    for (const existing of ["0x30", "0b110000"]) {
      const input = assDocument([`Style: Default,Arial,${existing},0,0`]);
      const result = applyStyleEdit(input, {
        fontSize: { enabled: true, targetSize: 48 },
      });
      expect(result.plan.changedStyleCount, existing).toBe(1);
      expect(result.content, existing).toContain("Style: Default,Arial,48,0,0");
    }
  });

  it("allows a valid Format-only style section as an honest zero-row plan", () => {
    const input = assDocument([]);
    const plan = planStyleEdit(input, EDIT_BOTH);
    expect(plan).toMatchObject({ styleCount: 0, changedStyleCount: 0, changedFieldCount: 0 });
  });
});

describe("inspectStyleDocument", () => {
  it("validates and counts styles without requiring an enabled operation", () => {
    const input = [
      "[V4+ Styles]",
      "Format: Name, Fontname, Fontsize",
      "Style: Default,Arial,48",
      "Style: Signs,Noto Sans,36",
    ].join("\n");
    expect(inspectStyleDocument(input)).toEqual({ styleCount: 2 });
  });

  it("accepts a structurally valid Name-only style format during ingestion", () => {
    const input = "[V4 Styles]\r\nFormat: Name\r\nStyle: Default";
    expect(inspectStyleDocument(input)).toEqual({ styleCount: 1 });

    expect(() =>
      planStyleEdit(input, {
        fontFamily: { enabled: true, targetFamily: "Microsoft YaHei" },
      })
    ).toThrow(/requires a Fontname column/);
  });

  it("retains the planner's strict structural errors and caps", () => {
    expect(() => inspectStyleDocument("[V4+ Styles]\nStyle: Default")).toThrow(/before.*Format/i);
    expect(() => inspectStyleDocument("[V4+ Styles]\nFormat: Name\nFormat: Name")).toThrow(
      /already has a Format record/
    );
  });
});

describe("strict structure and operation errors", () => {
  it("rejects a file without a V4 style section", () => {
    expect(() => planStyleEdit("[Script Info]\nTitle: none\n[Events]\n", EDIT_BOTH)).toThrow(
      /no \[V4\+ Styles\] or \[V4 Styles\]/
    );
  });

  it("rejects duplicate V4 style sections", () => {
    const input = `${assDocument(["Style: Default,Arial,20,0,0"])}\n[V4 Styles]\nFormat: Name, Fontname, Fontsize`;
    expect(() => planStyleEdit(input, EDIT_BOTH)).toThrow(/more than one V4 style section/);
  });

  it("rejects Style records before Format", () => {
    const input = "[V4+ Styles]\nStyle: Default,Arial,20\nFormat: Name, Fontname, Fontsize";
    expect(() => planStyleEdit(input, EDIT_BOTH)).toThrow(/before.*Format/i);
  });

  it("rejects missing and duplicate Format records", () => {
    expect(() => planStyleEdit("[V4+ Styles]\n; empty\n[Events]\n", EDIT_BOTH)).toThrow(
      /no Format record/
    );
    const duplicate =
      "[V4+ Styles]\nFormat: Name, Fontname, Fontsize\nFormat: Name, Fontname, Fontsize";
    expect(() => planStyleEdit(duplicate, EDIT_BOTH)).toThrow(/already has a Format record/);
  });

  it("rejects empty, duplicate, and operation-required Format columns", () => {
    const empty = "[V4+ Styles]\nFormat: Name,,Fontname\n";
    expect(() =>
      planStyleEdit(empty, { fontFamily: { enabled: true, targetFamily: "Arial" } })
    ).toThrow(/empty column/);

    const duplicate = "[V4+ Styles]\nFormat: Name, Fontname, FONTNAME\n";
    expect(() =>
      planStyleEdit(duplicate, { fontFamily: { enabled: true, targetFamily: "Arial" } })
    ).toThrow(/duplicate column names/);

    const missingFamily = "[V4+ Styles]\nFormat: Name, Fontsize\nStyle: Default,20";
    expect(() => planStyleEdit(missingFamily, EDIT_BOTH)).toThrow(/requires a Fontname/);

    const missingSize = "[V4+ Styles]\nFormat: Name, Fontname\nStyle: Default,Arial";
    expect(() => planStyleEdit(missingSize, EDIT_BOTH)).toThrow(/requires a Fontsize/);
  });

  it("bounds and scrubs Format column names without echoing them", () => {
    const longColumn = `[V4+ Styles]\nFormat: Name, ${"x".repeat(129)}`;
    expect(() =>
      planStyleEdit(longColumn, { fontSize: { enabled: true, targetSize: 48 } })
    ).toThrow(/Format column exceeds 128/);

    const spoofed = "[V4+ Styles]\nFormat: Name, Fontname\u202E, Fontsize";
    expect(() => planStyleEdit(spoofed, EDIT_BOTH)).toThrow(/column with control or invisible/);

    const duplicated = "[V4+ Styles]\nFormat: Name, SpoofMe, spoofme";
    let message = "";
    try {
      planStyleEdit(duplicated, { fontSize: { enabled: true, targetSize: 48 } });
    } catch (error) {
      message = error instanceof Error ? error.message : String(error);
    }
    expect(message).toContain("duplicate column names");
    expect(message.toLowerCase()).not.toContain("spoofme");
  });

  it("checks raw Format and Style fields before trimming invisible suffixes", () => {
    const operations: StyleEditOperations = {
      fontFamily: { enabled: true, targetFamily: "Microsoft YaHei" },
    };
    for (const suffix of ["\t", "\uFEFF"]) {
      const format = `[V4+ Styles]\nFormat: Name, Fontname${suffix}\nStyle: Default,Arial`;
      expect(() => planStyleEdit(format, operations), `Format suffix ${suffix}`).toThrow(
        /Format record contains a column with control or invisible/
      );

      for (const style of [
        `Style: Default${suffix},Arial,48`,
        `Style: Default,Arial${suffix},48`,
        `Style: Default,Arial,48${suffix}`,
      ]) {
        const input = `[V4+ Styles]\nFormat: Name, Fontname, Fontsize\n${style}`;
        expect(() => planStyleEdit(input, EDIT_BOTH), `Style suffix ${suffix}`).toThrow(
          /contains control or invisible/
        );
      }
    }
  });

  it("does not treat Unicode whitespace as a hidden record prefix", () => {
    const operations: StyleEditOperations = {
      fontFamily: { enabled: true, targetFamily: "Microsoft YaHei" },
    };

    for (const hiddenWhitespace of ["\uFEFF", "\u2028"]) {
      const hiddenFormat = `[V4+ Styles]\n${hiddenWhitespace}Format: Name, Fontname\nStyle: Default,Arial`;
      expect(() => planStyleEdit(hiddenFormat, operations)).toThrow(/before.*Format/i);

      const hiddenStyle = `[V4+ Styles]\nFormat: Name, Fontname\n${hiddenWhitespace}Style: Default,Arial`;
      const result = applyStyleEdit(hiddenStyle, operations);
      expect(result.plan.styleCount).toBe(0);
      expect(result.content).toBe(hiddenStyle);
    }
  });

  it("continues to allow ordinary ASCII space padding around Style fields", () => {
    const input = "[V4+ Styles]\nFormat: Name, Fontname, Fontsize\nStyle:  Default  , Arial , 48 ";
    const output = applyStyleEdit(input, EDIT_BOTH).content;
    expect(output).toContain("Style:  Default  , Microsoft YaHei , 52 ");
  });

  it("rejects Style rows whose field count differs from Format", () => {
    const tooFew = "[V4+ Styles]\nFormat: Name, Fontname, Fontsize\nStyle: Default,Arial";
    expect(() => planStyleEdit(tooFew, EDIT_BOTH)).toThrow(/2 fields but Format declares 3/);
    const tooMany = "[V4+ Styles]\nFormat: Name, Fontname, Fontsize\nStyle: Default,Arial,20,extra";
    expect(() => planStyleEdit(tooMany, EDIT_BOTH)).toThrow(/4 fields but Format declares 3/);
  });

  it("rejects unsafe family inputs and out-of-range font sizes", () => {
    const input = assDocument(["Style: Default,Arial,20,0,0"]);
    for (const family of ["", " Arial", "Arial,Other", "Arial\u202EOther"]) {
      expect(() =>
        planStyleEdit(input, { fontFamily: { enabled: true, targetFamily: family } })
      ).toThrow(StyleEditError);
    }
    for (const size of [0, 201, Number.NaN, Number.POSITIVE_INFINITY]) {
      expect(() => planStyleEdit(input, { fontSize: { enabled: true, targetSize: size } })).toThrow(
        /finite number from 1 to 200/
      );
    }
  });

  it("requires at least one enabled operation at the pure boundary", () => {
    const input = assDocument(["Style: Default,Arial,20,0,0"]);
    expect(() => planStyleEdit(input, {})).toThrow(/Enable at least one/);
    expect(() =>
      planStyleEdit(input, {
        fontFamily: { enabled: false, targetFamily: "" },
        fontSize: { enabled: false, targetSize: Number.NaN },
      })
    ).toThrow(/Enable at least one/);
  });

  it("bounds and scrubs existing Fontsize before preview or numeric conversion", () => {
    const longSize = assDocument([`Style: Default,Arial,${"9".repeat(129)},0,0`]);
    expect(() => planStyleEdit(longSize, EDIT_BOTH)).toThrow(/font size exceeds 128/);

    const spoofedSize = assDocument(["Style: Default,Arial,48\u202E,0,0"]);
    expect(() => planStyleEdit(spoofedSize, EDIT_BOTH)).toThrow(
      /font size contains control or invisible/
    );

    const repairable = assDocument(["Style: Default,Arial,not-a-number,0,0"]);
    expect(applyStyleEdit(repairable, EDIT_BOTH).content).toContain(
      "Style: Default,Microsoft YaHei,52,0,0"
    );
  });

  it("preserves a leading BOM when rewriting the first Style record", () => {
    const input =
      "\uFEFF[V4+ Styles]\r\nFormat: Name, Fontname, Fontsize\r\n" + "Style: Default,Arial,20";
    const output = applyStyleEdit(input, EDIT_BOTH).content;
    expect(output.charCodeAt(0)).toBe(0xfeff);
    expect(output).toBe(
      "\uFEFF[V4+ Styles]\r\nFormat: Name, Fontname, Fontsize\r\n" +
        "Style: Default,Microsoft YaHei,52"
    );
  });

  it("rejects a stale or invented row selection", () => {
    const input = assDocument(["Style: Default,Arial,20,0,0"]);
    expect(() => applyStyleEdit(input, EDIT_BOTH, ["style:invented"])).toThrow(
      /does not exist in the current plan/
    );
  });
});

describe("resource ceilings", () => {
  it("rejects an overlong physical line before style parsing", () => {
    const input = `[Script Info]\n${"x".repeat(1_000_001)}\n[V4+ Styles]`;
    expect(() => planStyleEdit(input, EDIT_BOTH)).toThrow(/Line 2: Line has 1000001 characters/);
  });

  it("rejects too many physical lines without materializing the whole split", () => {
    const input = "\n".repeat(501_024);
    expect(() => planStyleEdit(input, EDIT_BOTH)).toThrow(/more than 501024 lines/);
  });

  it("rejects an excessive Format field vector", () => {
    const input = `[V4+ Styles]\nFormat: ${Array.from({ length: 1_025 }, (_, i) => `F${i}`).join(
      ","
    )}`;
    expect(() => planStyleEdit(input, { fontSize: { enabled: true, targetSize: 48 } })).toThrow(
      /1025 fields \(max 1024\)/
    );
  });

  it("rejects more than 50000 style rows", () => {
    const rows = Array.from({ length: 50_001 }, (_, i) => `Style: S${i},Arial,20`).join("\n");
    const input = `[V4+ Styles]\nFormat: Name, Fontname, Fontsize\n${rows}`;
    expect(() => planStyleEdit(input, EDIT_BOTH)).toThrow(/more than 50000 Style records/);
  });
});

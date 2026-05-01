/**
 * Tests for analyzeFonts match-order behavior:
 * a user-supplied local font must win over a system match for the same
 * (family, bold, italic) tuple, and missing fonts must surface with
 * source=null.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import type { LocalFontEntry } from "../../lib/tauri-api";

// Mock the Tauri IPC surface so the test runs in pure Node. We only need
// findSystemFont for the fallback branch; subsetFont is unused here.
const findSystemFontMock = vi.fn();
vi.mock("../../lib/tauri-api", () => ({
  findSystemFont: (family: string, bold: boolean, italic: boolean) =>
    findSystemFontMock(family, bold, italic),
  subsetFont: vi.fn(),
}));

// Import after vi.mock so the mocked module is picked up.
import { analyzeFonts, buildUserFontMap, userFontKey } from "./font-embedder";

const MINIMAL_ASS = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,FZLanTingHei,40,0,0
Style: Emphasis,Arial,40,1,0

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Default,Hello
Dialogue: 0,0:00:05.00,0:00:10.00,Emphasis,Bold text
`;

function makeEntry(family: string, bold: boolean, italic: boolean, path: string): LocalFontEntry {
  return {
    path,
    index: 0,
    families: [family],
    bold,
    italic,
    sizeBytes: 1024,
  };
}

function makeMultiFamilyEntry(
  families: string[],
  bold: boolean,
  italic: boolean,
  path: string
): LocalFontEntry {
  return {
    path,
    index: 0,
    families,
    bold,
    italic,
    sizeBytes: 1024,
  };
}

describe("analyzeFonts — match priority", () => {
  beforeEach(() => {
    findSystemFontMock.mockReset();
  });

  it("skips findSystemFont when a local font matches (and reports source=local)", async () => {
    // System would have returned something, but the user's map wins first.
    findSystemFontMock.mockResolvedValue({ path: "C:/Windows/Fonts/FZ.ttf", index: 0 });
    const userFontMap = new Map<string, LocalFontEntry>();
    userFontMap.set(
      userFontKey("FZLanTingHei", false, false),
      makeEntry("FZLanTingHei", false, false, "C:/user/Fonts/FZ.ttf")
    );

    const { infos } = await analyzeFonts(MINIMAL_ASS, userFontMap);
    const fz = infos.find((i) => i.key.family === "FZLanTingHei");

    expect(fz).toBeDefined();
    expect(fz?.source).toBe("local");
    expect(fz?.filePath).toBe("C:/user/Fonts/FZ.ttf");
    // Critically: findSystemFont should not have been called for FZ — the
    // local match short-circuits the lookup.
    const fzCalls = findSystemFontMock.mock.calls.filter((c) => c[0] === "FZLanTingHei");
    expect(fzCalls.length).toBe(0);
  });

  it("falls back to system font when the family is not in the user map", async () => {
    // Per-family resolution rather than a single mockResolvedValue so the
    // mock can distinguish which font each invocation is for. The single-
    // value form would pass even if FZLanTingHei resolved through Arial's
    // path (or vice versa).
    findSystemFontMock.mockImplementation(async (family: string, bold: boolean) => {
      if (family === "FZLanTingHei") return { path: "C:/Windows/Fonts/simsun.ttc", index: 0 };
      if (family === "Arial" && bold) return { path: "C:/Windows/Fonts/arialbd.ttf", index: 0 };
      throw new Error(`unexpected lookup: ${family} bold=${bold}`);
    });
    const userFontMap = new Map<string, LocalFontEntry>();

    const { infos } = await analyzeFonts(MINIMAL_ASS, userFontMap);
    const arial = infos.find((i) => i.key.family === "Arial" && i.key.bold);
    const fz = infos.find((i) => i.key.family === "FZLanTingHei");

    expect(arial?.source).toBe("system");
    expect(arial?.filePath).toBe("C:/Windows/Fonts/arialbd.ttf");
    expect(fz?.source).toBe("system");
    expect(fz?.filePath).toBe("C:/Windows/Fonts/simsun.ttc");
  });

  it("marks unresolved fonts with source=null", async () => {
    findSystemFontMock.mockRejectedValue(new Error("Font not found"));
    const { infos } = await analyzeFonts(MINIMAL_ASS);

    for (const info of infos) {
      expect(info.source).toBeNull();
      expect(info.filePath).toBeNull();
      expect(info.error).toContain("Font not found");
    }
  });

  it("distinguishes bold and non-bold variants of the same family", async () => {
    const userFontMap = new Map<string, LocalFontEntry>();
    userFontMap.set(
      userFontKey("Arial", true, false),
      makeEntry("Arial", true, false, "C:/user/ArialBold.ttf")
    );
    // Regular Arial is not in the user map → must fall back.
    findSystemFontMock.mockImplementation(async (family: string, bold: boolean) => {
      if (family === "FZLanTingHei") return { path: "C:/Windows/Fonts/FZ.ttf", index: 0 };
      if (family === "Arial" && !bold) return { path: "C:/Windows/Fonts/arial.ttf", index: 0 };
      throw new Error("Font not found");
    });

    const { infos } = await analyzeFonts(MINIMAL_ASS, userFontMap);
    const arialBold = infos.find((i) => i.key.family === "Arial" && i.key.bold);
    expect(arialBold?.source).toBe("local");
    expect(arialBold?.filePath).toBe("C:/user/ArialBold.ttf");
  });
});

describe("analyzeFonts — real-world anime-release scenario", () => {
  beforeEach(() => {
    findSystemFontMock.mockReset();
  });

  // Simulates a real-world raw-pack scan output — ONE entry per face, with
  // every localized family-name variant packed inside the `families` array.
  const SCAN_OUTPUT: LocalFontEntry[] = [
    makeMultiFamilyEntry(["方正粗黑宋简体", "FZCuHeiSongS-B-GB"], false, false, "C:/R/FZCHSJW.TTF"),
    makeMultiFamilyEntry(
      ["HYXuanSong 75S", "HYXuanSong", "汉仪玄宋 75S", "汉仪玄宋"],
      false,
      false,
      "C:/R/HYXuanSong75S.ttf"
    ),
    makeMultiFamilyEntry(["青鸟华光简粗黑", "JCUH"], false, false, "C:/R/青鸟华光简粗黑.TTF"),
  ];

  const ANIME_ASS = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: CN ED,青鸟华光简粗黑,42,0,0
Style: CN OP,方正粗黑宋简体,54,0,0
Style: Default,HYXuanSong 75S,80,0,0

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,CN ED,blue
Dialogue: 0,0:00:05.00,0:00:10.00,CN OP,red
Dialogue: 0,0:00:10.00,0:00:15.00,Default,green
`;

  it("matches all three families across mixed-case extensions and localized names", async () => {
    // System lookup should not be hit for any of these — all three resolve locally.
    findSystemFontMock.mockRejectedValue(new Error("should not be called"));

    const map = buildUserFontMap(SCAN_OUTPUT);

    const { infos } = await analyzeFonts(ANIME_ASS, map);

    // Find each family by name and verify it resolved locally.
    for (const family of ["青鸟华光简粗黑", "方正粗黑宋简体", "HYXuanSong 75S"]) {
      const info = infos.find((i) => i.key.family === family);
      expect(info, `'${family}' not among infos`).toBeDefined();
      expect(info?.source, `'${family}' should be local`).toBe("local");
      expect(info?.filePath, `'${family}' should have a file path`).toBeTruthy();
    }
  });

  it("strips the ASS @ vertical-writing prefix so @Foo resolves the same file as Foo", async () => {
    // Regression test for a real bug: a `\fn@青鸟华光简粗黑` override tag
    // produced a lookup key '@青鸟华光简粗黑|0|0' that never matched
    // '青鸟华光简粗黑|0|0' in the user font map.
    findSystemFontMock.mockRejectedValue(new Error("should not be called"));

    const map = buildUserFontMap(SCAN_OUTPUT);

    const ASS_WITH_VERTICAL_OVERRIDE = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,HYXuanSong 75S,40,0,0

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Default,{\\fn@青鸟华光简粗黑}vertical
Dialogue: 0,0:00:05.00,0:00:10.00,Default,{\\fn青鸟华光简粗黑}horizontal
`;

    const { infos } = await analyzeFonts(ASS_WITH_VERTICAL_OVERRIDE, map);

    // Both vertical and horizontal references must collapse to a single
    // '青鸟华光简粗黑' usage — otherwise we'd emit two embed entries with
    // partial glyph coverage for the same font file.
    const qingniao = infos.filter((i) => i.key.family.endsWith("青鸟华光简粗黑"));
    expect(qingniao.length).toBe(1);
    expect(qingniao[0].key.family).toBe("青鸟华光简粗黑"); // no @ prefix
    expect(qingniao[0].source).toBe("local");
  });

  it("exposes every localized variant as a lookup key while keeping entry count = face count", () => {
    // 3 faces in, 8 variants across them.
    expect(SCAN_OUTPUT.length).toBe(3);
    const totalVariants = SCAN_OUTPUT.reduce((sum, e) => sum + e.families.length, 0);
    expect(totalVariants).toBe(8);

    const map = buildUserFontMap(SCAN_OUTPUT);
    expect(map.size).toBe(8);

    // Every family variant should resolve to the correct face.
    expect(map.get(userFontKey("HYXuanSong 75S", false, false))?.path).toContain("HYXuanSong75S");
    expect(map.get(userFontKey("汉仪玄宋", false, false))?.path).toContain("HYXuanSong75S");
    expect(map.get(userFontKey("青鸟华光简粗黑", false, false))?.path).toContain(
      "青鸟华光简粗黑.TTF"
    );
    expect(map.get(userFontKey("JCUH", false, false))?.path).toContain("青鸟华光简粗黑.TTF");
  });
});

describe("userFontKey", () => {
  it("lowercases family for case-insensitive matching", () => {
    expect(userFontKey("Arial", false, false)).toBe(userFontKey("ARIAL", false, false));
    expect(userFontKey("Arial", false, false)).toBe(userFontKey("arial", false, false));
  });

  it("encodes bold and italic flags distinctly", () => {
    const plain = userFontKey("Arial", false, false);
    const bold = userFontKey("Arial", true, false);
    const italic = userFontKey("Arial", false, true);
    const both = userFontKey("Arial", true, true);
    expect(new Set([plain, bold, italic, both]).size).toBe(4);
  });
});

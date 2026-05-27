/**
 * Tests for analyzeFonts match-order behavior:
 * a user-supplied local font must win over a system match for the same
 * (family, bold, italic) tuple, and missing fonts must surface with
 * source=null.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import type { LocalFontEntry } from "../../lib/tauri-api";

// Mock the Tauri IPC surface so the test runs in pure Node. findSystemFont
// covers the system-fallback branch; resolveUserFont covers the
// useRustUserFonts production path; subsetFont is unused here.
const findSystemFontMock = vi.fn();
const resolveUserFontMock = vi.fn();
const lookupFontFamilyMock = vi.fn();
vi.mock("../../lib/tauri-api", () => ({
  findSystemFont: (family: string, bold: boolean, italic: boolean) =>
    findSystemFontMock(family, bold, italic),
  resolveUserFont: (family: string, bold: boolean, italic: boolean) =>
    resolveUserFontMock(family, bold, italic),
  lookupFontFamily: (family: string, bold: boolean, italic: boolean) =>
    lookupFontFamilyMock(family, bold, italic),
  subsetFont: vi.fn(),
}));

// Import after vi.mock so the mocked module is picked up.
import {
  analyzeFonts,
  buildUserFontMap,
  embedFonts,
  userFontKey,
  MAX_SUBSET_CODEPOINTS_FOR_DEDUP,
  type FontInfo,
} from "./font-embedder";
import type { FontUsage } from "./font-collector";

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
    // Reset both mocks even though tests in this block pass userFontMap
    // (truthy), short-circuiting before resolveUserFont fires. Defensive
    // — keeps state symmetric so a future test added here that flips
    // useRustUserFonts=true doesn't inherit stale mock state.
    resolveUserFontMock.mockReset();
    // Cache lookup (#5) is called unconditionally by analyzeFonts;
    // default to null so tests not exercising the cache fall through
    // to the system-font path as before.
    lookupFontFamilyMock.mockReset();
    lookupFontFamilyMock.mockResolvedValue(null);
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
    // useRustUserFonts defaults to false in this call; verify the Rust
    // resolver was NOT consulted on the no-userFontMap path. Catches a
    // regression that flipped the default to true (which would change
    // production behavior for callers passing only positional args).
    expect(resolveUserFontMock).not.toHaveBeenCalled();
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
    // Counter-assertion : the bold-variant key hit
    // the user map, so findSystemFont was NOT consulted for Arial
    // (only for FZLanTingHei). Pins the variant-keyed priority: a
    // regression that dropped the bold bit from userFontKey would
    // miss the map and fall through to findSystemFont here.
    // `mock.calls` is typed as `any[][]` (vitest doesn't narrow the
    // arg shape); index-access instead of tuple-destructure to keep
    // tsc happy under noUncheckedIndexedAccess.
    const arialSystemCalls = findSystemFontMock.mock.calls.filter(
      (call: unknown[]) => call[0] === "Arial"
    );
    expect(arialSystemCalls).toHaveLength(0);
  });
});

describe("analyzeFonts — real-world anime-release scenario", () => {
  beforeEach(() => {
    findSystemFontMock.mockReset();
    // Reset both mocks even though tests in this block pass userFontMap
    // (truthy), short-circuiting before resolveUserFont fires. Defensive
    // — keeps state symmetric so a future test added here that flips
    // useRustUserFonts=true doesn't inherit stale mock state.
    resolveUserFontMock.mockReset();
    // Cache lookup (#5) is called unconditionally by analyzeFonts;
    // default to null so tests not exercising the cache fall through
    // to the system-font path as before.
    lookupFontFamilyMock.mockReset();
    lookupFontFamilyMock.mockResolvedValue(null);
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
    expect(qingniao[0]!.key.family).toBe("青鸟华光简粗黑"); // no @ prefix
    expect(qingniao[0]!.source).toBe("local");
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

  it("matches full face aliases without weakening ordinary family style matching", () => {
    const dreamHanW22: LocalFontEntry = {
      path: "D:/Fonts/DreamHanSerif-W22.ttc",
      index: 2,
      families: ["Dream Han Serif SC", "梦源宋体 SC"],
      faceNames: ["Dream Han Serif SC W22", "DreamHanSerifSC-W22"],
      bold: true,
      italic: false,
      sizeBytes: 42_000_000,
    };

    const map = buildUserFontMap([dreamHanW22]);

    expect(map.get(userFontKey("Dream Han Serif SC W22", false, false))?.path).toBe(
      "D:/Fonts/DreamHanSerif-W22.ttc"
    );
    expect(map.get(userFontKey("DreamHanSerifSC-W22", false, true))?.index).toBe(2);
    expect(map.get(userFontKey("Dream Han Serif SC", true, false))?.path).toBe(
      "D:/Fonts/DreamHanSerif-W22.ttc"
    );
    expect(map.get(userFontKey("Dream Han Serif SC", false, false))).toBeUndefined();
  });

  it("keeps exact family matches ahead of full-face aliases from other faces", () => {
    const exactFamily: LocalFontEntry = {
      path: "D:/Fonts/ExactSharedSans-Regular.otf",
      index: 0,
      families: ["Shared Sans"],
      bold: false,
      italic: false,
      sizeBytes: 1_000_000,
    };
    const aliasFace: LocalFontEntry = {
      path: "D:/Fonts/AliasFace-Bold.otf",
      index: 0,
      families: ["Other Sans"],
      faceNames: ["Shared Sans"],
      bold: true,
      italic: false,
      sizeBytes: 1_000_000,
    };

    const map = buildUserFontMap([exactFamily, aliasFace]);

    expect(map.get(userFontKey("Shared Sans", false, false))?.path).toBe(
      "D:/Fonts/ExactSharedSans-Regular.otf"
    );
    expect(map.get(userFontKey("Shared Sans", true, true))?.path).toBe(
      "D:/Fonts/AliasFace-Bold.otf"
    );
  });
});

describe("analyzeFonts — useRustUserFonts production path", () => {
  beforeEach(() => {
    findSystemFontMock.mockReset();
    resolveUserFontMock.mockReset();
    lookupFontFamilyMock.mockReset();
    lookupFontFamilyMock.mockResolvedValue(null);
  });

  // Covers the production code path that ships in FontEmbed batch mode:
  // userFontMap is null and analyzeFonts asks Rust's session-local index
  // via resolveUserFont. The legacy in-memory userFontMap branch is
  // covered by the suites above; this suite asserts the IPC contract
  // doesn't drift (e.g., field rename on either side).

  it("uses resolveUserFont as the local-source check when userFontMap is null", async () => {
    resolveUserFontMock.mockImplementation(async (family: string) => {
      if (family === "FZLanTingHei") return { path: "C:/u/FZ.ttf", index: 0 };
      return null;
    });
    findSystemFontMock.mockImplementation(async (family: string, bold: boolean) => {
      if (family === "Arial" && bold) return { path: "C:/Windows/Fonts/arialbd.ttf", index: 0 };
      throw new Error("Font not found");
    });

    const { infos } = await analyzeFonts(MINIMAL_ASS, null, undefined, true);
    const fz = infos.find((i) => i.key.family === "FZLanTingHei");
    const arial = infos.find((i) => i.key.family === "Arial" && i.key.bold);

    // Local hit via Rust IPC short-circuits the system call.
    expect(fz?.source).toBe("local");
    expect(fz?.filePath).toBe("C:/u/FZ.ttf");
    const fzSystemCalls = findSystemFontMock.mock.calls.filter((c) => c[0] === "FZLanTingHei");
    expect(fzSystemCalls.length).toBe(0);

    // Miss falls through to system.
    expect(arial?.source).toBe("system");
    expect(arial?.filePath).toBe("C:/Windows/Fonts/arialbd.ttf");
  });

  it("falls through to system when resolveUserFont returns null for every family", async () => {
    resolveUserFontMock.mockResolvedValue(null);
    findSystemFontMock.mockImplementation(async (family: string, bold: boolean) => {
      if (family === "FZLanTingHei") return { path: "C:/Windows/Fonts/simsun.ttc", index: 0 };
      if (family === "Arial" && bold) return { path: "C:/Windows/Fonts/arialbd.ttf", index: 0 };
      throw new Error("Font not found");
    });

    const { infos } = await analyzeFonts(MINIMAL_ASS, null, undefined, true);
    expect(infos.find((i) => i.key.family === "FZLanTingHei")?.source).toBe("system");
    expect(infos.find((i) => i.key.family === "Arial" && i.key.bold)?.source).toBe("system");
    // resolveUserFont was consulted for every distinct (family, bold, italic).
    // Pin BOTH the count AND which arguments — a regression that called
    // FZLanTingHei twice and Arial zero times would still satisfy a
    // count-only assertion.
    expect(resolveUserFontMock.mock.calls.length).toBe(2);
    expect(resolveUserFontMock).toHaveBeenCalledWith("FZLanTingHei", false, false);
    expect(resolveUserFontMock).toHaveBeenCalledWith("Arial", true, false);
  });

  it("userFontMap wins over useRustUserFonts when both are provided", async () => {
    // Production currently never calls analyzeFonts with both arguments
    // truthy; this test pins the priority order anyway so a future
    // caller that does pass both knows userFontMap takes precedence.
    // Without the anchor, a future refactor that flipped the
    // short-circuit order would silently change the priority.
    findSystemFontMock.mockResolvedValue({ path: "C:/Windows/Fonts/sys.ttf", index: 0 });
    // Scoped mock: return FZ only for the FZ family, null for Arial.
    // A mock returning the same FZ value for ANY family would let
    // Arial resolve via the Rust user-font tier with the wrong path,
    // masking a regression where userFontMap accidentally
    // short-circuited Arial too. With scoped mocks the Arial
    // counter-assertion below pins source="system".
    resolveUserFontMock.mockImplementation((family: string, bold: boolean, italic: boolean) => {
      if (family === "FZLanTingHei" && !bold && !italic) {
        return Promise.resolve({ path: "C:/rust-side/FZ.ttf", index: 0 });
      }
      return Promise.resolve(null);
    });
    const userFontMap = new Map<string, LocalFontEntry>();
    userFontMap.set(
      userFontKey("FZLanTingHei", false, false),
      makeEntry("FZLanTingHei", false, false, "C:/user-map/FZ.ttf")
    );

    const { infos } = await analyzeFonts(MINIMAL_ASS, userFontMap, undefined, true);
    const fz = infos.find((i) => i.key.family === "FZLanTingHei");

    expect(fz?.source).toBe("local");
    expect(fz?.filePath).toBe("C:/user-map/FZ.ttf");
    // resolveUserFont must NOT have been consulted for FZ — userFontMap
    // already short-circuited the lookup.
    const fzRustCalls = resolveUserFontMock.mock.calls.filter((c) => c[0] === "FZLanTingHei");
    expect(fzRustCalls.length).toBe(0);

    // Counter-assertion on Arial. MINIMAL_ASS declares two styles
    // (FZLanTingHei + Arial); without this pin, a
    // future regression that fed BOTH families through userFontMap
    // (silently dropping Arial because the map has no entry for it)
    // would still pass the FZ assertions above. Arial must resolve
    // via findSystemFont (system tier) and land with source="system".
    const arial = infos.find((i) => i.key.family === "Arial");
    expect(arial?.source).toBe("system");
    expect(arial?.filePath).toBe("C:/Windows/Fonts/sys.ttf");
  });

  // Positive coverage for the persistent-cache lookup tier. Previous
  // suite asserted MISSING-cache (`mockResolvedValue(null)`)
  // and the fall-through, but never the resolved-via-cache branch. Without
  // this test a regression that swapped the source label ("cache" → "system")
  // or dropped the cache tier entirely would still pass every existing test.
  it("resolves via persistent cache when session DB misses but cache hits", async () => {
    resolveUserFontMock.mockResolvedValue(null);
    lookupFontFamilyMock.mockImplementation(async (family: string, bold: boolean) => {
      if (family === "FZLanTingHei" && !bold) {
        return { path: "C:/cache/FZ.ttf", index: 0 };
      }
      return null;
    });
    findSystemFontMock.mockImplementation(async (family: string, bold: boolean) => {
      if (family === "Arial" && bold) return { path: "C:/Windows/Fonts/arialbd.ttf", index: 0 };
      throw new Error("Font not found");
    });

    const { infos } = await analyzeFonts(MINIMAL_ASS, null, undefined, true);
    const fz = infos.find((i) => i.key.family === "FZLanTingHei");
    const arial = infos.find((i) => i.key.family === "Arial" && i.key.bold);

    // Cache hit: source labeled "cache" (NOT "local" — that's the session
    // DB tier — and NOT "system"). filePath comes from the cache row.
    expect(fz?.source).toBe("cache");
    expect(fz?.filePath).toBe("C:/cache/FZ.ttf");
    // findSystemFont must NOT have been consulted for FZ — the cache
    // tier short-circuits the system lookup.
    const fzSystemCalls = findSystemFontMock.mock.calls.filter((c) => c[0] === "FZLanTingHei");
    expect(fzSystemCalls.length).toBe(0);

    // Cache miss for Arial falls through to system, as before.
    expect(arial?.source).toBe("system");
    expect(arial?.filePath).toBe("C:/Windows/Fonts/arialbd.ttf");
  });

  it("falls through to system fonts when persistent cache lookup rejects", async () => {
    resolveUserFontMock.mockResolvedValue(null);
    lookupFontFamilyMock.mockRejectedValue(new Error("corrupt cache"));
    findSystemFontMock.mockImplementation(async (family: string, bold: boolean) => {
      if (family === "FZLanTingHei" && !bold) return { path: "C:/Windows/Fonts/FZ.ttf", index: 0 };
      if (family === "Arial" && bold) return { path: "C:/Windows/Fonts/arialbd.ttf", index: 0 };
      throw new Error("Font not found");
    });

    const { infos } = await analyzeFonts(MINIMAL_ASS, null, undefined, true);
    const fz = infos.find((i) => i.key.family === "FZLanTingHei");
    const arial = infos.find((i) => i.key.family === "Arial" && i.key.bold);

    expect(fz?.source).toBe("system");
    expect(fz?.filePath).toBe("C:/Windows/Fonts/FZ.ttf");
    expect(arial?.source).toBe("system");
    expect(arial?.filePath).toBe("C:/Windows/Fonts/arialbd.ttf");
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

  it("pins exact byte shape of bold/italic flag positions (Wave 7.8 / N3-R7-6)", () => {
    // Distinctness via Set size alone isn't enough — a refactor that
    // swapped bold and italic positions (or used a different
    // separator) would still keep the 4 keys distinct but
    // would break the cross-layer pin with the Rust side's
    // `user_font_key` which uses `family|0|0`-shape encoding. The
    // exact-shape assertions below catch position swaps + separator
    // drift in one test. Aligned with the Rust counterpart at
    // `fonts.rs::user_font_key_separator_pin` (which pins
    // `arial\u{001F}0\u{001F}0`).
    // Separator is U+001F (Unit Separator), matching the Rust side's
    // exact-shape pin at `fonts.rs::user_font_key_separator_pin`.
    expect(userFontKey("Arial", false, false)).toBe("arial\u001f0\u001f0");
    expect(userFontKey("Arial", true, false)).toBe("arial\u001f1\u001f0");
    expect(userFontKey("Arial", false, true)).toBe("arial\u001f0\u001f1");
    expect(userFontKey("Arial", true, true)).toBe("arial\u001f1\u001f1");
  });
});

describe("MAX_SUBSET_CODEPOINTS_FOR_DEDUP value pin", () => {
  // TS-side mirror of the Rust test
  // `dedup_cap_matches_ipc_cap` in `bin/cli/main.rs::mod tests`.
  // The trinity that must stay in lockstep:
  //   1. `app_lib::fonts::MAX_SUBSET_CODEPOINTS` (Rust IPC cap)
  //   2. CLI `MAX_SUBSET_CODEPOINTS_FOR_DEDUP` in `bin/cli/main.rs`
  //   3. this TS `MAX_SUBSET_CODEPOINTS_FOR_DEDUP` in `font-embedder.ts`
  // The Rust test pins equality between (1) and (2) at the Rust
  // compile / test boundary. TS cannot import the Rust constant, so
  // the structural mirror is a literal pin: if you change this TS
  // value, this test goes red and forces you to update the Rust
  // source-of-truth (1) AND CLI value (2) in the same diff and
  // rerun the Rust `dedup_cap_matches_ipc_cap` test to confirm the
  // full trinity is realigned.
  it("equals 200_000 (the Rust IPC cap source of truth)", () => {
    expect(MAX_SUBSET_CODEPOINTS_FOR_DEDUP).toBe(200_000);
  });
});

describe("embedFonts — face dedup", () => {
  let subsetFontMock: ReturnType<typeof vi.fn>;
  beforeEach(async () => {
    const tauriApi = await import("../../lib/tauri-api");
    subsetFontMock = vi.mocked(tauriApi.subsetFont);
    subsetFontMock.mockReset();
  });

  // Minimal valid ASS with [Script Info] header — embedFonts calls
  // assertAssShape upfront and rejects anything that doesn't have
  // a [Script Info] section before [Events].
  const SHELL_ASS = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize
Style: Default,Microsoft YaHei,40
Style: Alt,微软雅黑,40

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Default,Hello
Dialogue: 0,0:00:05.00,0:00:10.00,Alt,你好
`;

  function makeInfo(family: string, filePath: string, fontIndex: number): FontInfo {
    return {
      key: { family, bold: false, italic: false },
      glyphCount: 5,
      filePath,
      fontIndex,
      error: null,
      source: "local",
    };
  }
  function makeUsage(family: string, codepoints: number[]): FontUsage {
    return {
      key: { family, bold: false, italic: false },
      codepoints: new Set(codepoints),
    };
  }

  it("collapses aliases that resolve to the same (filePath, fontIndex) into one [Fonts] entry", async () => {
    subsetFontMock.mockResolvedValue(new Uint8Array([1, 2, 3, 4]));

    const aliasA = makeInfo("Microsoft YaHei", "C:/Windows/Fonts/msyh.ttc", 0);
    const aliasB = makeInfo("微软雅黑", "C:/Windows/Fonts/msyh.ttc", 0);
    const usages = [
      makeUsage("Microsoft YaHei", [0x41, 0x42]),
      makeUsage("微软雅黑", [0x4f60, 0x597d]),
    ];

    const result = await embedFonts(SHELL_ASS, [aliasA, aliasB], usages);

    expect(result).not.toBeNull();
    expect(result!.embeddedCount).toBe(1);

    // subset_font called once per unique resolved face, not once
    // per alias. Pre-fix, this was called twice with byte-identical
    // payloads embedded under different filenames.
    expect(subsetFontMock).toHaveBeenCalledTimes(1);

    // Union of codepoints from both aliases passed to the single
    // subset call.
    const passedCodepoints = subsetFontMock.mock.calls[0]![2] as number[];
    expect(new Set(passedCodepoints)).toEqual(new Set([0x41, 0x42, 0x4f60, 0x597d]));

    // The resulting [Fonts] section has exactly one entry. The first
    // alias (English "Microsoft YaHei") wins the filename template.
    const fontnameMatches = result!.content.match(/^fontname:/gm) ?? [];
    expect(fontnameMatches.length).toBe(1);
  });

  it("keeps distinct entries for different fontIndex values (TTC face 0 vs face 1)", async () => {
    subsetFontMock.mockResolvedValue(new Uint8Array([1, 2, 3, 4]));

    // Same filePath, DIFFERENT fontIndex — genuinely different faces
    // of the same TTC; must NOT dedup.
    const face0 = makeInfo("Microsoft YaHei", "C:/Windows/Fonts/msyh.ttc", 0);
    const face1 = makeInfo("Microsoft YaHei UI", "C:/Windows/Fonts/msyh.ttc", 1);
    const usages = [makeUsage("Microsoft YaHei", [0x41]), makeUsage("Microsoft YaHei UI", [0x42])];

    const result = await embedFonts(SHELL_ASS, [face0, face1], usages);

    expect(result).not.toBeNull();
    expect(result!.embeddedCount).toBe(2);
    expect(subsetFontMock).toHaveBeenCalledTimes(2);
    const indicesPassed = subsetFontMock.mock.calls.map((call) => call[1] as number);
    expect(new Set(indicesPassed)).toEqual(new Set([0, 1]));
  });

  it("keeps distinct face tuples whose path-suffix digits and fontIndex digits adjoin (R1 N-R1-1 — Pattern 1 separator parity with userFontKey)", async () => {
    // Pre-fix the dedup key was built with FACE_DEDUP_SEP="" (or
    // equivalently a separator a reader could mistake for empty); two
    // distinct face tuples could collide on concatenation. After the
    // fix the separator is U+001F (Unit Separator) — same convention
    // as userFontKey (this file) and user_font_key (fonts.rs). This
    // test pins the no-collision contract using the canonical example:
    // ("foo.ttc", index=11, false, false) and ("foo1.ttc", index=1,
    // false, false) — under the old empty-separator concatenation
    // both folded to "foo.ttc1100" / "foo1.ttc100" respectively
    // (still distinct here, but the principle is keep-distinct
    // regardless of digit-boundary adjacency).
    subsetFontMock.mockResolvedValue(new Uint8Array([1, 2, 3, 4]));

    const aliasA = makeInfo("Family A", "C:/Windows/Fonts/foo.ttc", 11);
    const aliasB = makeInfo("Family B", "C:/Windows/Fonts/foo1.ttc", 1);
    const usages = [makeUsage("Family A", [0x41]), makeUsage("Family B", [0x42])];

    const result = await embedFonts(SHELL_ASS, [aliasA, aliasB], usages);

    expect(result).not.toBeNull();
    expect(result!.embeddedCount).toBe(2);
    expect(subsetFontMock).toHaveBeenCalledTimes(2);
    const callPaths = subsetFontMock.mock.calls.map((call) => call[0] as string);
    expect(new Set(callPaths)).toEqual(
      new Set(["C:/Windows/Fonts/foo.ttc", "C:/Windows/Fonts/foo1.ttc"])
    );
  });

  it("keeps distinct entries for different bold/italic styles on the same face", async () => {
    subsetFontMock.mockResolvedValue(new Uint8Array([1, 2, 3, 4]));

    // Same filePath + fontIndex but different bold flags — logically
    // distinct rendering targets (libass treats them as separate
    // picks); must NOT dedup. The dedup key includes bold/italic so
    // a future style refactor can't silently merge them.
    const plain: FontInfo = {
      key: { family: "Microsoft YaHei", bold: false, italic: false },
      glyphCount: 1,
      filePath: "C:/Windows/Fonts/msyh.ttc",
      fontIndex: 0,
      error: null,
      source: "local",
    };
    const bold: FontInfo = {
      key: { family: "Microsoft YaHei", bold: true, italic: false },
      glyphCount: 1,
      filePath: "C:/Windows/Fonts/msyh.ttc",
      fontIndex: 0,
      error: null,
      source: "local",
    };
    const usages: FontUsage[] = [
      { key: plain.key, codepoints: new Set([0x41]) },
      { key: bold.key, codepoints: new Set([0x42]) },
    ];

    const result = await embedFonts(SHELL_ASS, [plain, bold], usages);

    expect(result).not.toBeNull();
    expect(result!.embeddedCount).toBe(2);
    expect(subsetFontMock).toHaveBeenCalledTimes(2);
  });

  it("returns subset failure warnings while embedding the remaining fonts", async () => {
    subsetFontMock.mockImplementation(async (path: string) => {
      if (path.includes("broken")) throw new Error("broken font");
      return new Uint8Array([1, 2, 3, 4]);
    });

    const good = makeInfo("Good", "/fonts/good.ttf", 0);
    const broken = makeInfo("Broken", "/fonts/broken.ttf", 0);
    const usages = [makeUsage("Good", [0x41]), makeUsage("Broken", [0x42])];

    const result = await embedFonts(SHELL_ASS, [good, broken], usages);

    expect(result).not.toBeNull();
    expect(result!.embeddedCount).toBe(1);
    expect(result!.warnings).toContain("Skipped Broken: broken font");
    expect(subsetFontMock).toHaveBeenCalledTimes(2);
  });

  it("returns no-usage warnings instead of leaving them as transient progress only", async () => {
    subsetFontMock.mockResolvedValue(new Uint8Array([1, 2, 3, 4]));

    const orphan = makeInfo("MissingUsage", "/fonts/missing-usage.ttf", 0);
    const result = await embedFonts(SHELL_ASS, [orphan], []);

    expect(result).not.toBeNull();
    expect(result!.embeddedCount).toBe(0);
    expect(result!.warnings).toContain("Skipped MissingUsage: no usage entry");
    expect(subsetFontMock).not.toHaveBeenCalled();
  });
});

describe("embedFonts — face dedup cap boundary", () => {
  let subsetFontMock: ReturnType<typeof vi.fn>;
  beforeEach(async () => {
    const tauriApi = await import("../../lib/tauri-api");
    subsetFontMock = vi.mocked(tauriApi.subsetFont);
    subsetFontMock.mockReset();
  });

  const SHELL_ASS_2 = `[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize
Style: Default,Microsoft YaHei,40

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:05.00,Default,Hello
`;

  function makeInfoCap(family: string, filePath: string, fontIndex: number): FontInfo {
    return {
      key: { family, bold: false, italic: false },
      glyphCount: 5,
      filePath,
      fontIndex,
      error: null,
      source: "local",
    };
  }

  // Build a Set of `count` consecutive codepoints starting at `start`.
  // Used to produce DISJOINT ranges across aliases so the merged union
  // size equals the sum of per-alias sizes — without this, the test
  // can't pin the boundary because overlapping codepoints would
  // coalesce on union and the cap check would never fire.
  function makeCodepointRange(start: number, count: number): Set<number> {
    const out = new Set<number>();
    for (let i = 0; i < count; i++) out.add(start + i);
    return out;
  }

  it("merged union at MAX_SUBSET_CODEPOINTS goes through dedup (one subsetFont call)", async () => {
    subsetFontMock.mockResolvedValue(new Uint8Array([1, 2, 3, 4]));

    // 4 aliases × 50,000 disjoint codepoints each = 200,000 union,
    // exactly at the cap. Dedup decision uses `> cap`, so at-cap
    // takes the dedup path: one subsetFont call with the unioned
    // codepoints.
    const aliases = [
      makeInfoCap("A", "/fonts/face.ttf", 0),
      makeInfoCap("B", "/fonts/face.ttf", 0),
      makeInfoCap("C", "/fonts/face.ttf", 0),
      makeInfoCap("D", "/fonts/face.ttf", 0),
    ];
    const usages = aliases.map((info, k) => ({
      key: info.key,
      codepoints: makeCodepointRange(0x010000 + k * 50_000, 50_000),
    }));

    const result = await embedFonts(SHELL_ASS_2, aliases, usages);

    expect(result).not.toBeNull();
    expect(result!.embeddedCount).toBe(1);
    expect(subsetFontMock).toHaveBeenCalledTimes(1);
    const codepointsArg = subsetFontMock.mock.calls[0]![2] as number[];
    expect(codepointsArg.length).toBe(200_000);
  });

  it("merged union over MAX_SUBSET_CODEPOINTS falls back to per-alias subsetting", async () => {
    subsetFontMock.mockResolvedValue(new Uint8Array([1, 2, 3, 4]));

    // 4 aliases × 50,001 disjoint codepoints each = 200,004 union,
    // one codepoint over the cap. Dedup is skipped for this face;
    // each alias is subset separately, producing 4 [Fonts] entries
    // (pre-2a-i shape, scoped to this group only).
    const aliases = [
      makeInfoCap("A", "/fonts/face.ttf", 0),
      makeInfoCap("B", "/fonts/face.ttf", 0),
      makeInfoCap("C", "/fonts/face.ttf", 0),
      makeInfoCap("D", "/fonts/face.ttf", 0),
    ];
    const usages = aliases.map((info, k) => ({
      key: info.key,
      codepoints: makeCodepointRange(0x010000 + k * 50_001, 50_001),
    }));

    const result = await embedFonts(SHELL_ASS_2, aliases, usages);

    expect(result).not.toBeNull();
    expect(result!.embeddedCount).toBe(4);
    expect(subsetFontMock).toHaveBeenCalledTimes(4);
    // Each per-alias call gets exactly that alias's codepoints —
    // 50,001 — which is well under both the per-variant cap (65,536)
    // and the subset cap (200,000), so no individual subset call
    // could itself overflow.
    for (const call of subsetFontMock.mock.calls) {
      const codepoints = call[2] as number[];
      expect(codepoints.length).toBe(50_001);
    }
  });

  it("merged union at exactly MAX_SUBSET_CODEPOINTS + 1 falls back to per-alias", async () => {
    // Off-by-one boundary pin: a regression flipping `>` to `>=` on
    // `mergedCodepoints.size > MAX_SUBSET_CODEPOINTS_FOR_DEDUP`
    // would silently bail dedup one entry early — both this test
    // (size = cap + 1) and the at-cap test above (size = cap, takes
    // dedup) light up. The earlier "over by 4" test wouldn't
    // distinguish a `>=` regression because both flavors of the
    // condition agree at 200004.
    subsetFontMock.mockResolvedValue(new Uint8Array([1, 2, 3, 4]));

    // 50,001 + 50,000 × 3 = 200,001 (cap + 1). Offsets are well
    // separated to keep ranges disjoint.
    const aliases = [
      makeInfoCap("A", "/fonts/face.ttf", 0),
      makeInfoCap("B", "/fonts/face.ttf", 0),
      makeInfoCap("C", "/fonts/face.ttf", 0),
      makeInfoCap("D", "/fonts/face.ttf", 0),
    ];
    const sizes = [50_001, 50_000, 50_000, 50_000];
    const usages = aliases.map((info, k) => ({
      key: info.key,
      codepoints: makeCodepointRange(0x010000 + k * 0x010000, sizes[k]!),
    }));

    const result = await embedFonts(SHELL_ASS_2, aliases, usages);

    expect(result).not.toBeNull();
    expect(result!.embeddedCount).toBe(4);
    // Fallback path: one subset call per alias, not one dedup call.
    expect(subsetFontMock).toHaveBeenCalledTimes(4);
  });
});

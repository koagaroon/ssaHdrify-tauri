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
    // Counter-assertion (N-R5-FECHAIN-10): the bold-variant key hit
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
    // Scoped mock (Round 6 Wave 6.6 #25): return FZ only for the FZ
    // family, null for Arial. Pre-W6.6 the mock returned the same FZ
    // value for ANY family — Arial would then resolve via the Rust
    // user-font tier with the wrong path, masking a regression where
    // userFontMap accidentally short-circuited Arial too. With scoped
    // mocks the Arial counter-assertion below pins source="system".
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

    // Round 6 Wave 6.6 #25 — counter-assertion on Arial. MINIMAL_ASS
    // declares two styles (FZLanTingHei + Arial); without this pin, a
    // future regression that fed BOTH families through userFontMap
    // (silently dropping Arial because the map has no entry for it)
    // would still pass the FZ assertions above. Arial must resolve
    // via findSystemFont (system tier) and land with source="system".
    const arial = infos.find((i) => i.key.family === "Arial");
    expect(arial?.source).toBe("system");
    expect(arial?.filePath).toBe("C:/Windows/Fonts/sys.ttf");
  });

  // Round 1 F3.N-R1-21: positive coverage for the persistent-cache
  // lookup tier. Previous suite asserted MISSING-cache (`mockResolvedValue(null)`)
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

import { describe, expect, it } from "vitest";

import {
  WINDOWS_RESERVED_NAMES,
  assertSafeOutputFilename,
  assertSafeOutputPath,
} from "./path-validation";

describe("assertSafeOutputFilename", () => {
  it("accepts ordinary filenames", () => {
    expect(() => assertSafeOutputFilename("episode01.ass")).not.toThrow();
    expect(() => assertSafeOutputFilename("EP01.shifted.ass")).not.toThrow();
    expect(() => assertSafeOutputFilename("show.s01e01.embed.ass")).not.toThrow();
  });

  it("rejects empty / whitespace-only filenames", () => {
    expect(() => assertSafeOutputFilename("")).toThrow(/empty/);
    expect(() => assertSafeOutputFilename("   ")).toThrow(/empty/);
  });

  it("rejects path separators in the filename", () => {
    expect(() => assertSafeOutputFilename("dir/file.ass")).toThrow(/illegal/);
    expect(() => assertSafeOutputFilename("dir\\file.ass")).toThrow(/illegal/);
  });

  it("rejects Windows-illegal punctuation", () => {
    for (const ch of '<>:"|?*') {
      expect(() => assertSafeOutputFilename(`x${ch}y.ass`)).toThrow(/illegal/);
    }
  });

  it("rejects control characters and DEL", () => {
    expect(() => assertSafeOutputFilename("x\x00y.ass")).toThrow(/illegal/);
    expect(() => assertSafeOutputFilename("x\x1fy.ass")).toThrow(/illegal/);
    expect(() => assertSafeOutputFilename("x\x7fy.ass")).toThrow(/illegal/);
  });

  it("rejects every Windows reserved name regardless of extension", () => {
    for (const name of WINDOWS_RESERVED_NAMES) {
      expect(() => assertSafeOutputFilename(`${name}.ass`)).toThrow(/reserved name/);
      expect(() => assertSafeOutputFilename(`${name}.shifted.ass`)).toThrow(/reserved name/);
    }
  });

  it("rejects reserved names with trailing whitespace / dots (Windows resolves them anyway)", () => {
    expect(() => assertSafeOutputFilename("CON .ass")).toThrow(/reserved name/);
    expect(() => assertSafeOutputFilename("CON. .ass")).toThrow(/reserved name/);
  });

  it("matches reserved names case-insensitively", () => {
    expect(() => assertSafeOutputFilename("con.ass")).toThrow(/reserved name/);
    expect(() => assertSafeOutputFilename("Con.ass")).toThrow(/reserved name/);
  });

  it("does not reject non-reserved names that share a prefix with reserved names", () => {
    expect(() => assertSafeOutputFilename("CONFIG.ass")).not.toThrow();
    expect(() => assertSafeOutputFilename("COM10.ass")).not.toThrow();
  });
});

describe("assertSafeOutputPath", () => {
  const inputBackslash = "C:\\subs\\episode01.ass";

  it("accepts a same-directory output path", () => {
    expect(() =>
      assertSafeOutputPath("C:/subs/episode01.shifted.ass", inputBackslash)
    ).not.toThrow();
  });

  it("rejects directory traversal segments", () => {
    expect(() => assertSafeOutputPath("C:/subs/../escape/episode01.ass", inputBackslash)).toThrow(
      /traversal/
    );
  });

  it("does not flag substring `..foo` (only the segment form is unsafe)", () => {
    // Construct a sibling whose name happens to contain `..` as a
    // non-segment substring. The traversal check is segment-anchored.
    expect(() => assertSafeOutputPath("C:/subs/show..ep01.ass", inputBackslash)).not.toThrow();
  });

  it("rejects paths that escape the input directory", () => {
    expect(() => assertSafeOutputPath("C:/other/episode01.ass", inputBackslash)).toThrow(/escapes/);
  });

  it("does not collapse `dir1` vs `dir12` due to prefix sharing", () => {
    // `subs` and `subs2` share a 4-char prefix. The dir-escape check
    // requires `subs/` boundary, so `subs2` is correctly flagged.
    expect(() => assertSafeOutputPath("C:/subs2/episode01.ass", inputBackslash)).toThrow(/escapes/);
  });

  it("rejects paths exceeding MAX_PATH (260 by default)", () => {
    const long = "C:/subs/" + "a".repeat(300) + ".ass";
    expect(() => assertSafeOutputPath(long, inputBackslash)).toThrow(/too long/);
  });

  it("relaxes the cap to 32767 for `\\\\?\\` long-local paths", () => {
    const longInput = "\\\\?\\C:\\subs\\episode01.ass";
    const longButOk = "//?/C:/subs/" + "a".repeat(500) + ".ass";
    expect(() => assertSafeOutputPath(longButOk, longInput)).not.toThrow();
  });

  it("keeps the 260 cap for UNC long paths (server may not support long paths)", () => {
    const uncInput = "\\\\?\\UNC\\server\\share\\subs\\episode01.ass";
    const longUnc = "//?/UNC/server/share/subs/" + "a".repeat(300) + ".ass";
    expect(() => assertSafeOutputPath(longUnc, uncInput)).toThrow(/too long/);
  });

  it("rejects self-overwrite when output basename only differs in case", () => {
    // Realistic scenario: user template produces an output basename
    // that case-folds to the input basename. The dir-escape check
    // (which is case-sensitive) doesn't fire because the directory
    // portion matches the input's case exactly. Self-overwrite check
    // is case-insensitive, so the upper-case basename collides.
    expect(() => assertSafeOutputPath("C:/subs/EPISODE01.ass", inputBackslash)).toThrow(
      /same as input/
    );
  });
});

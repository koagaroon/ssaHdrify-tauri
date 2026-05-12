import { describe, expect, it } from "vitest";

import {
  WINDOWS_RESERVED_NAMES,
  assertSafeOutputFilename,
  assertSafeOutputPath,
  decomposeInputPath,
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

  it("rejects COM0 / LPT0 (added per current MS spec)", () => {
    expect(() => assertSafeOutputFilename("COM0.ass")).toThrow(/reserved name/);
    expect(() => assertSafeOutputFilename("LPT0.ass")).toThrow(/reserved name/);
  });

  it("rejects COM/LPT superscript-digit variants (¹ ² ³)", () => {
    expect(() => assertSafeOutputFilename("COM¹.ass")).toThrow(/reserved name/);
    expect(() => assertSafeOutputFilename("COM².ass")).toThrow(/reserved name/);
    expect(() => assertSafeOutputFilename("COM³.ass")).toThrow(/reserved name/);
    expect(() => assertSafeOutputFilename("LPT¹.ass")).toThrow(/reserved name/);
    expect(() => assertSafeOutputFilename("LPT².ass")).toThrow(/reserved name/);
    expect(() => assertSafeOutputFilename("LPT³.ass")).toThrow(/reserved name/);
  });

  it("rejects { and } in filenames (catches unsubstituted template tokens)", () => {
    // `{Format}` typed instead of `{format}` would otherwise produce a
    // literal `episode.{Format}.ass` because the substitution path is
    // case-sensitive. Rejecting brace literals turns the typo into an
    // error.
    expect(() => assertSafeOutputFilename("episode.{Format}.ass")).toThrow(/illegal/);
    expect(() => assertSafeOutputFilename("a{b.ass")).toThrow(/illegal/);
    expect(() => assertSafeOutputFilename("a}b.ass")).toThrow(/illegal/);
  });
});

describe("decomposeInputPath", () => {
  it("decomposes a Windows drive-rooted path with subdirectory", () => {
    const parts = decomposeInputPath("C:\\subs\\episode.ass");
    expect(parts).toEqual({
      dir: "C:/subs",
      baseName: "episode",
      ext: ".ass",
      normalized: "C:/subs/episode.ass",
      usedBackslash: true,
    });
  });

  it("accepts a file at drive root (regression: Z:\\cat.ass)", () => {
    // The forum-tester bug repro: bare filename `cat.ass` from cwd
    // `Z:\` → Rust shell joins to `Z:\cat.ass` → engine resolver used
    // to reject because dir == "Z:" matched a stray `^[A-Za-z]:$/`
    // check that conflated drive-rooted with drive-relative.
    const parts = decomposeInputPath("Z:\\cat.ass");
    expect(parts).toEqual({
      dir: "Z:",
      baseName: "cat",
      ext: ".ass",
      normalized: "Z:/cat.ass",
      usedBackslash: true,
    });
  });

  it("accepts forward-slash drive-rooted path", () => {
    const parts = decomposeInputPath("C:/subs/episode.ass");
    expect(parts.usedBackslash).toBe(false);
    expect(parts.dir).toBe("C:/subs");
  });

  it("biases to backslash output for mixed-separator Windows paths", () => {
    // A Windows path that picked up a `/` from upstream JS normalization
    // should still output backslashes — `inputPath.includes("\\")` is
    // sufficient, no need for the older "no forward slash" form.
    const parts = decomposeInputPath("C:\\subs/episode.ass");
    expect(parts.usedBackslash).toBe(true);
  });

  it("accepts POSIX absolute paths", () => {
    const parts = decomposeInputPath("/home/user/episode.ass");
    expect(parts).toEqual({
      dir: "/home/user",
      baseName: "episode",
      ext: ".ass",
      normalized: "/home/user/episode.ass",
      usedBackslash: false,
    });
  });

  it("rejects bare filenames", () => {
    expect(() => decomposeInputPath("episode.ass")).toThrow(/must be absolute/);
  });

  it("rejects drive-relative paths (drive letter without separator)", () => {
    // `C:foo.ass` on Windows means "file foo.ass on drive C's CURRENT
    // directory" — drive-relative, ambiguous, must be rejected.
    expect(() => decomposeInputPath("C:episode.ass")).toThrow(/must be absolute/);
  });

  it("rejects empty input", () => {
    expect(() => decomposeInputPath("")).toThrow(/must be absolute/);
  });

  it("rejects control characters in the path", () => {
    expect(() => decomposeInputPath("C:\\subs\\evil\x00.ass")).toThrow(/control characters/);
    expect(() => decomposeInputPath("C:\\sub\x1fs\\episode.ass")).toThrow(/control characters/);
  });

  it("rejects empty filename component (trailing slash)", () => {
    expect(() => decomposeInputPath("C:/subs/")).toThrow(/no filename/);
  });

  it("rejects dots-only stem (e.g., '...')", () => {
    // `.ass` alone is treated as a hidden-file shape (baseName=".ass",
    // ext="") and accepted, consistent with `.bashrc` / `.gitignore`
    // semantics. Only a stem that strips entirely to nothing — `...`
    // → "" after `replace(/^\.+/, '')` — fails the valid-stem check.
    expect(() => decomposeInputPath("C:/subs/...")).toThrow(/no valid stem/);
  });

  it("preserves no-extension files", () => {
    const parts = decomposeInputPath("C:\\subs\\Makefile");
    expect(parts.baseName).toBe("Makefile");
    expect(parts.ext).toBe("");
  });

  it("treats hidden-file leading dots as part of the stem", () => {
    // `.hidden.ass` → baseName `.hidden`, ext `.ass`. The stem-validity
    // check (`replace(/^\.+/, '').trim()`) sees "hidden" — non-empty.
    const parts = decomposeInputPath("/etc/.hidden.ass");
    expect(parts.baseName).toBe(".hidden");
    expect(parts.ext).toBe(".ass");
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

  it("rejects paths exceeding the 259-char practical MAX_PATH limit", () => {
    // 259 = 260 buffer minus the trailing null terminator. A 260-char
    // path passes a naive `> 260` check but trips
    // ERROR_PATH_NOT_FOUND at write time on standard Windows APIs.
    const long = "C:/subs/" + "a".repeat(300) + ".ass";
    expect(() => assertSafeOutputPath(long, inputBackslash)).toThrow(/too long/);
  });

  it("relaxes the cap to 32766 for `\\\\?\\` long-local paths", () => {
    const longInput = "\\\\?\\C:\\subs\\episode01.ass";
    const longButOk = "//?/C:/subs/" + "a".repeat(500) + ".ass";
    expect(() => assertSafeOutputPath(longButOk, longInput)).not.toThrow();
  });

  it("keeps the 259 cap for UNC long paths (server may not support long paths)", () => {
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

describe("decomposeInputPath — Round 5 BiDi / zero-width hardening", () => {
  it("rejects paths containing BiDi RLO (Trojan-Source class)", () => {
    // EP01<U+202E>cssa.ass renders as EP01ssa.shifted.ass after the
    // RLO flip but lands on disk verbatim. Pre-Wave-5.1 the helper's
    // control-char regex was C0/DEL only and let BiDi through; the
    // shared unicode-controls set now catches it (N-R5-FELIB-06 +
    // A-R5-FELIB-01).
    expect(() => decomposeInputPath("C:/subs/EP01‮cssa.ass")).toThrow(/invisible|bidi/i);
  });

  it("rejects paths containing zero-width space (U+200B)", () => {
    expect(() => decomposeInputPath("C:/subs/EP​01.ass")).toThrow(/invisible|bidi/i);
  });

  it("rejects paths containing zero-width no-break space (U+FEFF / BOM-in-middle)", () => {
    expect(() => decomposeInputPath("/home/u/sub﻿s/episode.ass")).toThrow(/invisible|bidi/i);
  });

  it("rejects paths containing LRI / PDI isolate marks", () => {
    expect(() => decomposeInputPath("C:/subs/EP⁦01⁩.ass")).toThrow(/invisible|bidi/i);
  });

  it("accepts paths with no invisible / bidi characters", () => {
    expect(() => decomposeInputPath("C:/subs/episode-with-emoji-😀.ass")).not.toThrow();
    expect(() => decomposeInputPath("/home/u/CJK/字幕.ass")).not.toThrow();
  });
});

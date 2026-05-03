/**
 * Output naming tests — ported from Python tests/test_hdrify.py
 *
 * Tests cover: template resolution, tag stripping, path traversal
 * defense, Windows reserved names, and edge cases.
 */
import { describe, it, expect } from "vitest";
import { resolveOutputPath, DEFAULT_TEMPLATE } from "./output-naming";

// Use absolute paths since resolveOutputPath requires them
const BASE = "C:/movies";

describe("resolveOutputPath — template resolution", () => {
  it("default template produces {name}.hdr.ass", () => {
    const result = resolveOutputPath(`${BASE}/subtitle.srt`, DEFAULT_TEMPLATE, "PQ");
    expect(result).toBe(`${BASE}/subtitle.hdr.ass`);
  });

  it("substitutes {eotf} with lowercase EOTF name", () => {
    const result = resolveOutputPath(`${BASE}/subtitle.ass`, "{name}.{eotf}.ass", "HLG");
    expect(result).toBe(`${BASE}/subtitle.hlg.ass`);
  });

  it("strips existing .hdr tag to prevent double tagging", () => {
    // Use .srt extension so output differs from input (avoids self-overwrite guard)
    const result = resolveOutputPath(`${BASE}/subtitle.hdr.srt`, "{name}.hdr.ass", "PQ");
    // Should be subtitle.hdr.ass, NOT subtitle.hdr.hdr.ass
    expect(result).toBe(`${BASE}/subtitle.hdr.ass`);
  });

  it("preserves non-tag suffixes like .eng", () => {
    const result = resolveOutputPath(`${BASE}/subtitle.eng.srt`, "{name}.hdr.ass", "PQ");
    expect(result).toBe(`${BASE}/subtitle.eng.hdr.ass`);
  });

  it("strips stacked .hdr.sdr tags", () => {
    const result = resolveOutputPath(`${BASE}/subtitle.hdr.sdr.ass`, "{name}.hdr.ass", "PQ");
    expect(result).toBe(`${BASE}/subtitle.hdr.ass`);
  });
});

describe("resolveOutputPath — security", () => {
  // Anchor each rejection on the actual branch that fires. output-naming.ts
  // throws 12+ distinct messages, and a regression that broke the intended
  // safety branch but happened to fall into "Output filename must end with
  // .ass" or "empty filename" would otherwise pass a bare toThrow().
  //
  // Note on the traversal branches: with the current dot-collapse + illegal-
  // char gates upstream, the explicit `traversal:` and `escapes input
  // directory:` branches are unreachable by any caller-supplied template
  // (illegal-chars catches `/` first; the `\.{2,}` collapse erases `..`).
  // The tests anchor on the union /illegal|traversal|escapes/i so they
  // continue to verify "user-supplied `../...` templates are rejected
  // somewhere" without forcing assumptions about which branch.
  it("rejects path traversal with ../", () => {
    expect(() => resolveOutputPath(`${BASE}/sub/file.srt`, "../escape/{name}.ass", "PQ")).toThrow(
      /illegal|traversal|escapes/i
    );
  });

  it("rejects path traversal via prefix collision", () => {
    expect(() =>
      resolveOutputPath(`${BASE}/sub/file.srt`, "../subtitles/{name}.ass", "PQ")
    ).toThrow(/illegal|traversal|escapes/i);
  });

  it("rejects empty template", () => {
    expect(() => resolveOutputPath(`${BASE}/subtitle.srt`, "", "PQ")).toThrow(/empty/i);
  });

  it("rejects Windows reserved name CON", () => {
    expect(() => resolveOutputPath(`${BASE}/subtitle.srt`, "CON.ass", "PQ")).toThrow(/reserved/i);
  });

  it("rejects Windows reserved name with dollar sign (CONIN$)", () => {
    expect(() => resolveOutputPath(`${BASE}/subtitle.srt`, "CONIN$.ass", "PQ")).toThrow(
      /reserved/i
    );
  });

  it("rejects Windows reserved name with trailing space (CON .ass)", () => {
    expect(() => resolveOutputPath(`${BASE}/subtitle.srt`, "CON .ass", "PQ")).toThrow(/reserved/i);
  });

  it("rejects non-absolute input paths", () => {
    expect(() => resolveOutputPath("subtitle.srt", "{name}.hdr.ass", "PQ")).toThrow(/absolute/i);
  });

  it("rejects overwriting source file", () => {
    expect(() => resolveOutputPath(`${BASE}/subtitle.ass`, "{name}.ass", "PQ")).toThrow(
      /overwrite|same/i
    );
  });
});

describe("resolveOutputPath — {video_name} and {lang} tokens", () => {
  it("substitutes {video_name} from options, stripping extension", () => {
    const result = resolveOutputPath(`${BASE}/EP01.srt`, "{video_name}.ass", "PQ", {
      videoName: "Show.S01E01.1080p.mkv",
    });
    expect(result).toBe(`${BASE}/Show.S01E01.1080p.ass`);
  });

  it("accepts {video_name} without extension", () => {
    const result = resolveOutputPath(`${BASE}/EP01.srt`, "{video_name}.ass", "PQ", {
      videoName: "Show.S01E01",
    });
    expect(result).toBe(`${BASE}/Show.S01E01.ass`);
  });

  it("auto-extracts {lang} from filename's last dotted segment", () => {
    const result = resolveOutputPath(`${BASE}/movie.zh.srt`, "{name}.{lang}.ass", "PQ");
    // `{name}` resolves to `movie.zh` (full stem); `{lang}` resolves to `zh`
    expect(result).toBe(`${BASE}/movie.zh.zh.ass`);
  });

  it("explicit lang option overrides filename extraction", () => {
    const result = resolveOutputPath(`${BASE}/movie.en.srt`, "{video_name}.{lang}.ass", "PQ", {
      videoName: "Movie",
      lang: "zh",
    });
    expect(result).toBe(`${BASE}/Movie.zh.ass`);
  });

  it("collapses double dots when {lang} resolves empty in middle of template", () => {
    const result = resolveOutputPath(
      `${BASE}/movie.unknown_tag.srt`,
      "{video_name}.{lang}.ass",
      "PQ",
      { videoName: "Movie" }
    );
    // `unknown_tag` is not in LANG_TAGS → langValue=""; collapse `..` → `.`
    expect(result).toBe(`${BASE}/Movie.ass`);
    expect(result).not.toContain("Movie..ass");
  });

  it("recognizes common language tags case-insensitively", () => {
    const result = resolveOutputPath(`${BASE}/EP01.JA.srt`, "{video_name}.{lang}.ass", "PQ", {
      videoName: "Show.EP01",
    });
    expect(result).toBe(`${BASE}/Show.EP01.ja.ass`);
  });

  it("returns empty {lang} for filenames without dotted segments", () => {
    const result = resolveOutputPath(`${BASE}/EP01.srt`, "{video_name}.{lang}.ass", "PQ", {
      videoName: "Show",
    });
    // No {lang} match, no explicit option → empty → collapse
    expect(result).toBe(`${BASE}/Show.ass`);
  });

  it("treats {video_name} as empty when option omitted, collapsing dots", () => {
    const result = resolveOutputPath(`${BASE}/EP01.srt`, "{video_name}.{name}.ass", "PQ");
    // videoStem="" + "." + "EP01" + ".ass" → ".EP01.ass" → no collapse needed
    // (the leading dot is preserved as a hidden-file marker)
    expect(result).toBe(`${BASE}/.EP01.ass`);
  });

  it("paired Tab 4 default template produces clean output", () => {
    // Common Tab 4 case: pick one .zh.ass sub for a paired video.
    const result = resolveOutputPath(`${BASE}/EP01.zh.ass`, "{video_name}.{lang}.ass", "PQ", {
      videoName: "Show.S01E01.1080p.mkv",
    });
    expect(result).toBe(`${BASE}/Show.S01E01.1080p.zh.ass`);
  });
});

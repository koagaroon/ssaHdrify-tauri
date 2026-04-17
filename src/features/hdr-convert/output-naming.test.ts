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
    expect(result).toContain("subtitle.hdr.ass");
  });

  it("substitutes {eotf} with lowercase EOTF name", () => {
    const result = resolveOutputPath(`${BASE}/subtitle.ass`, "{name}.{eotf}.ass", "HLG");
    expect(result).toContain("subtitle.hlg.ass");
  });

  it("strips existing .hdr tag to prevent double tagging", () => {
    // Use .srt extension so output differs from input (avoids self-overwrite guard)
    const result = resolveOutputPath(`${BASE}/subtitle.hdr.srt`, "{name}.hdr.ass", "PQ");
    // Should be subtitle.hdr.ass, NOT subtitle.hdr.hdr.ass
    expect(result).toContain("subtitle.hdr.ass");
    expect(result).not.toContain("subtitle.hdr.hdr.ass");
  });

  it("preserves non-tag suffixes like .eng", () => {
    const result = resolveOutputPath(`${BASE}/subtitle.eng.srt`, "{name}.hdr.ass", "PQ");
    expect(result).toContain("subtitle.eng.hdr.ass");
  });

  it("strips stacked .hdr.sdr tags", () => {
    const result = resolveOutputPath(`${BASE}/subtitle.hdr.sdr.ass`, "{name}.hdr.ass", "PQ");
    expect(result).toContain("subtitle.hdr.ass");
    expect(result).not.toContain("subtitle.hdr.hdr.ass");
  });
});

describe("resolveOutputPath — security", () => {
  it("rejects path traversal with ../", () => {
    expect(() => resolveOutputPath(`${BASE}/sub/file.srt`, "../escape/{name}.ass", "PQ")).toThrow();
  });

  it("rejects path traversal via prefix collision", () => {
    expect(() =>
      resolveOutputPath(`${BASE}/sub/file.srt`, "../subtitles/{name}.ass", "PQ")
    ).toThrow();
  });

  it("rejects empty template", () => {
    expect(() => resolveOutputPath(`${BASE}/subtitle.srt`, "", "PQ")).toThrow();
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

import { describe, expect, it } from "vitest";

import { deriveStyledPath } from "./style-output";

describe("deriveStyledPath", () => {
  it("derives a safe sibling ASS output", () => {
    expect(deriveStyledPath("C:\\subs\\episode.ass")).toBe("C:\\subs\\episode.styled.ass");
  });

  it("preserves legacy SSA and extension casing", () => {
    expect(deriveStyledPath("C:\\subs\\episode.ssa")).toBe("C:\\subs\\episode.styled.ssa");
    expect(deriveStyledPath("C:\\subs\\episode.ASS")).toBe("C:\\subs\\episode.styled.ASS");
  });

  it("rejects unsupported extensions and non-absolute paths", () => {
    expect(() => deriveStyledPath("C:\\subs\\episode.srt")).toThrow(/\.ass or \.ssa/);
    expect(() => deriveStyledPath("episode.ass")).toThrow(/absolute/);
  });

  it("rejects an already-styled input instead of overwriting it or accumulating infixes", () => {
    expect(() => deriveStyledPath("C:\\subs\\episode.styled.ass")).toThrow(/same as input/);
  });

  it("rejects an empty stem after stripping the styled infix", () => {
    expect(() => deriveStyledPath("C:\\subs\\.styled.ass")).toThrow(/no valid stem/);
  });

  it("routes generated names through the shared reserved-name validator", () => {
    expect(() => deriveStyledPath("C:\\subs\\CON.ass")).toThrow(/reserved/i);
  });
});

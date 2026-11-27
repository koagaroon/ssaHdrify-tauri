import { describe, expect, it } from "vitest";
import { iterateSourceLines } from "./source-lines";

describe("iterateSourceLines", () => {
  it("yields nothing for empty input", () => {
    expect(Array.from(iterateSourceLines(""))).toEqual([]);
  });

  it("preserves exact LF, CRLF, and bare-CR spans and reconstructs the original string", () => {
    const sample = "alpha\nbeta\r\ngamma\rdelta";
    const lines = Array.from(iterateSourceLines(sample));
    expect(lines.map((l) => [l.body, l.ending])).toEqual([
      ["alpha", "\n"],
      ["beta", "\r\n"],
      ["gamma", "\r"],
      ["delta", ""],
    ]);
    for (const line of lines) {
      expect(sample.slice(line.start, line.end)).toBe(line.body + line.ending);
    }
    expect(lines.map((l) => l.body + l.ending).join("")).toBe(sample);
  });

  it("retains trailing terminators on the final non-empty line without inventing an extra empty line", () => {
    const sample = "first\r\nsecond\r\n";
    const lines = Array.from(iterateSourceLines(sample));
    expect(lines).toHaveLength(2);
    expect(lines[1]).toEqual({ start: 7, end: 15, body: "second", ending: "\r\n" });
  });
});

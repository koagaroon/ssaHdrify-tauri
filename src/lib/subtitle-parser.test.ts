import { describe, expect, it } from "vitest";
import { parseSubtitle } from "./subtitle-parser";

describe("parseSubtitle", () => {
  it("splits SRT cue blocks with mixed CRLF and LF endings", () => {
    const content =
      "1\r\n00:00:01,000 --> 00:00:02,000\r\nFirst line\r\n\n" +
      "2\n00:00:03,000 --> 00:00:04,000\nSecond line\n";
    const result = parseSubtitle(content);
    expect(result.format).toBe("srt");
    expect(result.captions).toHaveLength(2);
    expect(result.captions.map((c) => c.text)).toEqual(["First line", "Second line"]);
  });

  it("splits VTT cue blocks with mixed CRLF and LF endings", () => {
    const content =
      "WEBVTT\r\n\r\n" +
      "cue-a\r\n00:00:01.000 --> 00:00:02.000\r\nFirst line\r\n\n" +
      "cue-b\n00:00:03.000 --> 00:00:04.000\nSecond line\n";
    const result = parseSubtitle(content);
    expect(result.format).toBe("vtt");
    expect(result.captions).toHaveLength(2);
    expect(result.captions.map((c) => c.cueId)).toEqual(["cue-a", "cue-b"]);
    expect(result.captions.map((c) => c.text)).toEqual(["First line", "Second line"]);
  });

  it("parses ASS Dialogue lines and reports format=ass", () => {
    const content =
      "[Script Info]\nScriptType: v4.00+\n\n" +
      "[V4+ Styles]\nFormat: Name, Fontname\nStyle: Default,Arial\n\n" +
      "[Events]\nFormat: Layer, Start, End, Style, Text\n" +
      "Dialogue: 0,0:00:01.00,0:00:02.50,Default,Hello\n" +
      "Dialogue: 0,0:00:03.00,0:00:04.50,Default,World\n";
    const result = parseSubtitle(content);
    expect(result.format).toBe("ass");
    expect(result.captions).toHaveLength(2);
    // ASS timing only — the parseAss `text` field is the post-comma
    // remainder which still includes the Style field; the timing
    // operations don't care about text content. Anchor on the
    // load-bearing fields.
    expect(result.captions[0].start).toBe(1000);
    expect(result.captions[0].end).toBe(2500);
    expect(result.captions[1].start).toBe(3000);
    expect(result.captions[1].end).toBe(4500);
  });

  it("parses MicroDVD SUB frame ranges and reports format=sub", () => {
    // Frame numbers at 23.976 fps default. {24}{48} ≈ 1001 ms → 2002 ms;
    // {72}{96} ≈ 3003 ms → 4004 ms. The defining behavior of the SUB
    // parser is the frame-to-ms conversion, so anchor on the timing math
    // (not just the verbatim text slice).
    const content = "{24}{48}First frame block\n{72}{96}Second frame block\n";
    const result = parseSubtitle(content);
    expect(result.format).toBe("sub");
    expect(result.captions).toHaveLength(2);
    expect(result.captions[0].text).toBe("First frame block");
    expect(result.captions[1].text).toBe("Second frame block");
    // Exact integer assertions — parseSub does Math.round so the
    // output is always an integer ms. The previous toBeCloseTo(N, -1)
    // form was a 5 ms tolerance window (Vitest interprets numDigits as
    // 0.5 × 10^-N, so -1 gives ±5), which would let a Math.round →
    // Math.floor regression slip past silently.
    expect(result.captions[0].start).toBe(1001);
    expect(result.captions[0].end).toBe(2002);
    expect(result.captions[1].start).toBe(3003);
    expect(result.captions[1].end).toBe(4004);
  });

  it("throws when the content has no recognized header or timing", () => {
    // detectFormat returns "unknown"; parseSubtitle treats that as a
    // hard error so callers don't silently process zero-caption results.
    expect(() => parseSubtitle("just some prose\nwith no timing markers\nat all\n")).toThrow(
      /Could not detect/i
    );
  });

  it("parses ASS within the defensive entry cap", () => {
    // Defense-in-depth cap inside parseAss — guards against pathological
    // files (or runaway generators) that would otherwise fan out to
    // millions of caption objects in JS heap. The actual cap is
    // MAX_PARSED_ENTRIES (500_000); constructing 500k+1 dialogue lines
    // to exercise the throw branch is too slow for a unit test. This
    // test is a smoke guard that a well-formed in-cap file still parses
    // cleanly — the throw branch itself is unverified at the test
    // layer. (If a future contract regression flips the cap to a much
    // smaller number, that's the failure this guard catches by going
    // red on the 100-entry input.)
    const header =
      "[Script Info]\nScriptType: v4.00+\n\n" +
      "[V4+ Styles]\nFormat: Name, Fontname\nStyle: Default,Arial\n\n" +
      "[Events]\nFormat: Layer, Start, End, Style, Text\n";
    const smallBatch = Array.from(
      { length: 100 },
      (_, i) => `Dialogue: 0,0:00:00.00,0:00:00.10,Default,line ${i}`
    ).join("\n");
    const result = parseSubtitle(header + smallBatch + "\n");
    expect(result.format).toBe("ass");
    expect(result.captions).toHaveLength(100);
  });
});

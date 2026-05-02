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
});

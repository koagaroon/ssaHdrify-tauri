import { describe, expect, it } from "vitest";
import { parseSubtitle, shiftSubtitle } from "./subtitle-parser";

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

  it("smoke-tests a 100-entry ASS parse stays well within MAX_PARSED_ENTRIES", () => {
    // Defense-in-depth cap inside parseAss — guards against pathological
    // files (or runaway generators) that would otherwise fan out to
    // millions of caption objects in JS heap. The actual cap is
    // MAX_PARSED_ENTRIES (500_000); constructing 500k+1 dialogue lines
    // to exercise the throw branch is too slow for a unit test. This
    // test is a smoke guard that a well-formed in-cap file still parses
    // cleanly — the throw branch itself is unverified at the test
    // layer. If a future contract regression flips the cap to a much
    // smaller number (say, 50), that's the failure this guard catches.
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

  it("smoke-tests a 100-entry SRT parse stays well within MAX_PARSED_ENTRIES", () => {
    // SRT shares the same cap as ASS via a per-format check inside
    // parseSrt. A 100-entry block exercises the parser path without
    // approaching the cap; same regression-on-cap-shrink guard. SRT
    // canonical form uses a comma between seconds and milliseconds.
    const pad = (n: number) => n.toString().padStart(2, "0");
    const blocks = Array.from({ length: 100 }, (_, i) => {
      const start = `00:${pad(Math.floor(i / 60))}:${pad(i % 60)},000`;
      const end = `00:${pad(Math.floor((i + 1) / 60))}:${pad((i + 1) % 60)},000`;
      return `${i + 1}\n${start} --> ${end}\nline ${i}\n`;
    }).join("\n");
    const result = parseSubtitle(blocks);
    expect(result.format).toBe("srt");
    expect(result.captions).toHaveLength(100);
  });

  it("smoke-tests a 100-entry SUB parse stays well within MAX_PARSED_ENTRIES", () => {
    // MicroDVD SUB shares the same cap via parseSub. Frame numbers stay
    // small + bounded; same regression-on-cap-shrink guard.
    const lines = Array.from(
      { length: 100 },
      (_, i) => `{${i * 24}}{${(i + 1) * 24}}line ${i}`
    ).join("\n");
    const result = parseSubtitle(lines);
    expect(result.format).toBe("sub");
    expect(result.captions).toHaveLength(100);
  });
});

// ── Round 6 Wave 6.8 — Codex Finding 1 regression pin ──
//
// Junk-flood SRT/VTT — millions of non-cue blocks separated by blank
// lines — must trip the raw-block ceiling and abort the parse, NOT
// silently scan every block until the per-caption cap somehow fires
// (it never does; junk blocks skip the cap check via timingIdx === -1).
// W6.5 #18 introduced the regression; W6.8 added MAX_RAW_BLOCKS as
// defense-in-depth alongside MAX_PARSED_ENTRIES.
describe("parseSubtitle — Wave 6.8 raw-block junk-flood ceiling (Codex Finding 1)", () => {
  it("rejects SRT with > MAX_RAW_BLOCKS junk blocks before the parse loop scans them", () => {
    // One valid cue at the head so format detection fires SRT, followed
    // by junk blocks that have no timing line. Pre-W6.8 the parser
    // would scan every junk block via `if (timingIdx === -1) continue`
    // without counting them against MAX_PARSED_ENTRIES.
    //
    // splitCueBlocks splits on `\n[ \t]*\n` (blank line), so junk blocks
    // need DOUBLE newlines between them, not single. `"... block\n\n"`
    // repeated 2_000_001 times → 2_000_001 junk blocks + 1 valid =
    // crosses MAX_RAW_BLOCKS = 2_000_000.
    //
    // Construction is O(N) in memory; 2M tiny blocks = ~42 MB string —
    // acceptable for a single test, well below the 50 MB Rust read cap.
    const validCue = "1\n00:00:00,000 --> 00:00:01,000\nintro\n\n";
    const junk = "NOTE junk-only block\n\n";
    const blocks = validCue + junk.repeat(2_000_001);
    expect(() => parseSubtitle(blocks)).toThrow(/Too many subtitle blocks/);
  });

  it("rejects VTT with > MAX_RAW_BLOCKS junk blocks before the parse loop scans them", () => {
    // WEBVTT header for format detection, one real cue, then junk.
    // Same blank-line separator requirement as the SRT test.
    const head = "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nintro\n\n";
    const junk = "NOTE junk-only block\n\n";
    const blocks = head + junk.repeat(2_000_001);
    expect(() => parseSubtitle(blocks)).toThrow(/Too many subtitle blocks/);
  });

  it("accepts SRT with stray blank-line padding well below the raw-block ceiling", () => {
    // 100 valid cues + 100 blank padding blocks = 200 raw blocks, far
    // below the 2M ceiling — must not false-fail. Guards the regression
    // direction (W6.5 #18 was trying to fix exactly this shape).
    const pad = (n: number) => n.toString().padStart(2, "0");
    const cues = Array.from({ length: 100 }, (_, i) => {
      const start = `00:${pad(Math.floor(i / 60))}:${pad(i % 60)},000`;
      const end = `00:${pad(Math.floor((i + 1) / 60))}:${pad((i + 1) % 60)},000`;
      return `${i + 1}\n${start} --> ${end}\nline ${i}\n`;
    });
    // Double-blank between cues so splitCueBlocks reports padding-blocks.
    const withPadding = cues.join("\n\n");
    const result = parseSubtitle(withPadding);
    expect(result.format).toBe("srt");
    expect(result.captions).toHaveLength(100);
  });
});

// ── Round 7.5 follow-up — Codex finding (commit 7afe881) regression pin ──
//
// W7.5 A4-R7-4 introduced MAX_CAPTION_TEXT_LEN with a `continue` that
// silently dropped oversized ASS Dialogue lines from `captions`.
// buildAss still walked every original Dialogue regex match and
// consumed captions sequentially, so the next normal line received the
// oversized line's slot. Result for `shiftSubtitle` on a crafted ASS:
// silent timestamp drift across every Dialogue after the first
// oversized one. Fix: parseAss now emits a placeholder Caption with
// `skipped: true` for oversized lines; buildAss returns the original
// line untouched on that flag but still advances its index, keeping
// positional alignment.
describe("parseSubtitle / shiftSubtitle — oversized-ASS-Dialogue placeholder alignment", () => {
  it("preserves Dialogue order when an oversized line precedes normal lines", () => {
    const oversized = "X".repeat(65_000);
    const ass =
      "[Script Info]\nScriptType: v4.00+\n\n" +
      "[V4+ Styles]\nFormat: Name, Fontname\nStyle: Default,Arial\n\n" +
      "[Events]\nFormat: Layer, Start, End, Style, Text\n" +
      `Dialogue: 0,0:00:01.00,0:00:02.00,Default,${oversized}\n` +
      "Dialogue: 0,0:00:10.00,0:00:11.00,Default,SECOND\n" +
      "Dialogue: 0,0:00:20.00,0:00:21.00,Default,THIRD\n";

    const parsed = parseSubtitle(ass);
    expect(parsed.format).toBe("ass");
    // parseAss emits 3 captions (1 placeholder + 2 normal), preserving
    // positional alignment with the 3 original Dialogue lines.
    expect(parsed.captions).toHaveLength(3);
    expect(parsed.captions[0].skipped).toBe(true);
    expect(parsed.captions[0].text).toBe("");
    expect(parsed.captions[1].skipped).toBeUndefined();
    expect(parsed.captions[1].text).toContain("SECOND");
    expect(parsed.captions[2].text).toContain("THIRD");

    // Shift by +1s: the oversized original line must stay verbatim;
    // the next two lines must move from 10/11s → 11/12s and 20/21s →
    // 21/22s. A drift regression would write 11.00 onto the oversized
    // line and shift SECOND / THIRD by one slot each.
    const { output } = shiftSubtitle(ass, 1000);
    expect(output).toContain(`Dialogue: 0,0:00:01.00,0:00:02.00,Default,${oversized}`);
    expect(output).toContain("Dialogue: 0,0:00:11.00,0:00:12.00,Default,SECOND");
    expect(output).toContain("Dialogue: 0,0:00:21.00,0:00:22.00,Default,THIRD");
    // Negative counter-assertions against the prior drift pattern:
    // SECOND must NOT carry the oversized line's pre-shift timestamps,
    // and THIRD must NOT carry SECOND's pre-shift timestamps.
    expect(output).not.toMatch(/Dialogue: 0,0:00:02\.00,0:00:03\.00,Default,SECOND/);
    expect(output).not.toMatch(/Dialogue: 0,0:00:11\.00,0:00:12\.00,Default,THIRD/);
  });

  it("preserves Dialogue order with a normal line between two oversized lines", () => {
    // Stress shape: oversized at the head AND in the middle. Without
    // placeholders the captions array would be length-2 (just MIDDLE
    // and LAST), buildAss would consume MIDDLE onto the first
    // oversized slot and LAST onto MIDDLE's slot, leaving the second
    // oversized slot untouched (idx exhausted). Drift surface: both
    // surviving lines mis-attributed.
    const big = "Y".repeat(65_000);
    const ass =
      "[Script Info]\nScriptType: v4.00+\n\n" +
      "[V4+ Styles]\nFormat: Name, Fontname\nStyle: Default,Arial\n\n" +
      "[Events]\nFormat: Layer, Start, End, Style, Text\n" +
      `Dialogue: 0,0:00:01.00,0:00:02.00,Default,${big}\n` +
      "Dialogue: 0,0:00:10.00,0:00:11.00,Default,MIDDLE\n" +
      `Dialogue: 0,0:00:30.00,0:00:31.00,Default,${big}\n` +
      "Dialogue: 0,0:00:40.00,0:00:41.00,Default,LAST\n";

    const { output } = shiftSubtitle(ass, 500);
    expect(output).toContain(`Dialogue: 0,0:00:01.00,0:00:02.00,Default,${big}`);
    expect(output).toContain("Dialogue: 0,0:00:10.50,0:00:11.50,Default,MIDDLE");
    expect(output).toContain(`Dialogue: 0,0:00:30.00,0:00:31.00,Default,${big}`);
    expect(output).toContain("Dialogue: 0,0:00:40.50,0:00:41.50,Default,LAST");
  });

  // ── Round 8 Wave 8.3 — parser boundary pins ──

  it("parses single-digit-hour VTT timing as zero (bounded-hour regex)", () => {
    // Round 8 A-R8-A1-2 / N-R8-N1-1: the VTT hour group is bounded
    // `\d{2,12}` now, so a stray single-digit hour like "1:00:00.000"
    // doesn't satisfy the HH:MM:SS form. The MM:SS arm still matches
    // ("1:00.000" is allowed by that arm), but a 3-component "1:00:00"
    // must NOT match HH:MM:SS. Pin the rejection.
    const content =
      "WEBVTT\r\n\r\n" + "1:00:00.000 --> 1:00:02.000\r\n" + "this should not match HH:MM:SS\r\n";
    const result = parseSubtitle(content);
    expect(result.format).toBe("vtt");
    // The cue's timing line failed to match → block skipped → zero captions.
    expect(result.captions).toHaveLength(0);
  });

  it("parses 12-digit-hour VTT timing (upper bound)", () => {
    // Round 10 N-R10-019: actually use a 12-digit fixture so the
    // at-limit test pins the boundary from the inside. The Round 9
    // attempt used a 9-digit fixture and called it "upper bound";
    // a regression lowering the bound to `\d{2,11}` would have left
    // both tests green (9 digits passes, 13 fails) without
    // exercising the actual 12-digit edge. The 12-digit fixture +
    // the 13-digit over-bound counter-test (below) pin the boundary
    // from both sides — code_review.md "boundary-named tests pair
    // at-limit + over-limit".
    const longHour = "999999999999"; // 12 digits, exactly at {2,12} upper bound
    const content = `WEBVTT\r\n\r\n${longHour}:00:01.000 --> ${longHour}:00:02.000\r\nLine\r\n`;
    const result = parseSubtitle(content);
    expect(result.captions).toHaveLength(1);
  });

  it("rejects 13-digit-hour VTT timing (upper bound enforced)", () => {
    // Round 9 N-R9-N1-3 companion: above-cap hour fails the HH:MM:SS
    // form (no MM:SS fallback matches a 13-digit prefix either), so
    // the cue is skipped and the parse yields zero captions.
    const tooLong = "9999999999999"; // 13 digits, exceeds {2,12}
    const content = `WEBVTT\r\n\r\n${tooLong}:00:01.000 --> ${tooLong}:00:02.000\r\nLine\r\n`;
    const result = parseSubtitle(content);
    expect(result.captions).toHaveLength(0);
  });
});

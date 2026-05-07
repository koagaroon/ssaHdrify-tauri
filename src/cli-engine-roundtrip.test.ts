/**
 * Round-trip GUI ↔ CLI byte-equivalence tests.
 *
 * Per the CLI design doc (Step 6): "same input + same arguments → GUI's
 * full-flow output and CLI's full-flow output must be byte-identical".
 * This file pins that contract so a future refactor that drifts the CLI
 * wrapper (default resolution, output-path computation, intermediate
 * argument shape) fails before it ships.
 *
 * Both sides import from the same shared engine modules
 * (hdr-convert/*, timing-shift/*), so byte equality is a structural
 * property — but only as long as both wrappers keep calling the same
 * functions with the same arguments. These tests assert that property
 * by replaying the GUI's call sequence inline and comparing.
 */
import { describe, it, expect } from "vitest";

import { convertHdr, convertShift } from "./cli-engine-entry";
import {
  DEFAULT_BRIGHTNESS,
  type Eotf,
} from "./features/hdr-convert/color-engine";
import { processAssContent } from "./features/hdr-convert/ass-processor";
import {
  DEFAULT_STYLE,
  buildAssDocument,
  isConvertible,
  isNativeAss,
  processSrtUserText,
} from "./features/hdr-convert/srt-converter";
import { DEFAULT_TEMPLATE, resolveOutputPath } from "./features/hdr-convert/output-naming";
import {
  deriveShiftedPath,
  shiftSubtitles,
} from "./features/timing-shift/timing-engine";
import { parseSubtitle } from "./lib/subtitle-parser";

// ── Fixtures ─────────────────────────────────────────────

const ASS_FIXTURE = [
  "[Script Info]",
  "ScriptType: v4.00+",
  "",
  "[V4+ Styles]",
  "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding",
  "Style: Default,Arial,48,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,10,1",
  "",
  "[Events]",
  "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text",
  "Dialogue: 0,0:00:00.00,0:00:05.00,Default,,0,0,0,,{\\c&H00FF00&}Hello world",
  "Dialogue: 0,0:00:05.00,0:00:10.00,Default,,0,0,0,,Plain dialogue line",
].join("\n");

const SRT_FIXTURE = [
  "1",
  "00:00:00,000 --> 00:00:01,500",
  "<font color=\"#FF0000\">Red</font> opening",
  "",
  "2",
  "00:00:01,500 --> 00:00:03,000",
  "Plain second line",
  "",
].join("\n");

// GUI replay of the HDR-Convert tab's per-file imperative flow
// (HdrConvert.tsx:365-397). Mirrors what processAssContent / SRT-path
// chain produces for a given (content, brightness, eotf) tuple.
function guiHdrFlow(
  inputPath: string,
  content: string,
  eotf: Eotf,
  brightness: number,
  outputTemplate: string
): { outputPath: string; content: string } {
  const fileName = inputPath.replace(/\\/g, "/").split("/").pop() ?? inputPath;
  const outputPath = resolveOutputPath(inputPath, outputTemplate, eotf);

  if (isNativeAss(fileName)) {
    return { outputPath, content: processAssContent(content, brightness, eotf) };
  }

  if (isConvertible(fileName)) {
    const preprocessed = processSrtUserText(content);
    const { captions } = parseSubtitle(preprocessed, DEFAULT_STYLE.fps);
    const rawAss = buildAssDocument(
      captions.map((c) => ({ start: c.start, end: c.end, text: c.text })),
      DEFAULT_STYLE
    );
    return { outputPath, content: processAssContent(rawAss, brightness, eotf) };
  }

  throw new Error(`Unsupported subtitle format: ${fileName}`);
}

describe("HDR convert — GUI ↔ CLI byte equivalence", () => {
  const inputAss = "C:\\subs\\episode01.ass";
  const inputSrt = "C:\\subs\\episode02.srt";

  it("ASS path produces byte-identical output for shared (content, brightness, eotf)", () => {
    const cli = convertHdr({
      inputPath: inputAss,
      content: ASS_FIXTURE,
      eotf: "pq",
      brightness: 1000,
      outputTemplate: DEFAULT_TEMPLATE,
    });
    const gui = guiHdrFlow(inputAss, ASS_FIXTURE, "pq", 1000, DEFAULT_TEMPLATE);

    expect(cli.outputPath).toBe(gui.outputPath);
    expect(cli.content).toBe(gui.content);
  });

  it("SRT path produces byte-identical output through the same conversion chain", () => {
    const cli = convertHdr({
      inputPath: inputSrt,
      content: SRT_FIXTURE,
      eotf: "hlg",
      brightness: 4000,
      outputTemplate: DEFAULT_TEMPLATE,
    });
    const gui = guiHdrFlow(inputSrt, SRT_FIXTURE, "hlg", 4000, DEFAULT_TEMPLATE);

    expect(cli.outputPath).toBe(gui.outputPath);
    expect(cli.content).toBe(gui.content);
  });

  it("CLI applies DEFAULT_BRIGHTNESS when brightness is omitted, matching GUI default state", () => {
    const cli = convertHdr({
      inputPath: inputAss,
      content: ASS_FIXTURE,
      eotf: "pq",
      outputTemplate: DEFAULT_TEMPLATE,
    });
    const gui = guiHdrFlow(inputAss, ASS_FIXTURE, "pq", DEFAULT_BRIGHTNESS, DEFAULT_TEMPLATE);

    expect(cli.content).toBe(gui.content);
  });
});

describe("Time shift — GUI ↔ CLI byte equivalence", () => {
  const input = "C:\\subs\\episode01.ass";

  it("default output path matches deriveShiftedPath exactly when no template is given", () => {
    const cli = convertShift({
      inputPath: input,
      content: ASS_FIXTURE,
      offsetMs: 2500,
    });
    const guiResult = shiftSubtitles(ASS_FIXTURE, { offsetMs: 2500 });
    const guiOutputPath = deriveShiftedPath(input);

    expect(cli.outputPath).toBe(guiOutputPath);
    expect(cli.content).toBe(guiResult.content);
    expect(cli.format).toBe(guiResult.format);
    expect(cli.captionCount).toBe(guiResult.captionCount);
  });

  it("threshold-gated shift produces same content as direct shiftSubtitles call", () => {
    const cli = convertShift({
      inputPath: input,
      content: ASS_FIXTURE,
      offsetMs: -500,
      thresholdMs: 5_000,
    });
    const gui = shiftSubtitles(ASS_FIXTURE, {
      offsetMs: -500,
      thresholdMs: 5_000,
    });

    expect(cli.content).toBe(gui.content);
    expect(cli.shiftedCount).toBe(gui.preview.filter((p) => p.wasShifted).length);
  });
});

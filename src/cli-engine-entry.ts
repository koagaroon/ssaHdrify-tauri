import { processAssContent } from "./features/hdr-convert/ass-processor";
import {
  DEFAULT_STYLE,
  buildAssDocument,
  isConvertible,
  isNativeAss,
  processSrtUserText,
} from "./features/hdr-convert/srt-converter";
import { DEFAULT_BRIGHTNESS, type Eotf } from "./features/hdr-convert/color-engine";
import { DEFAULT_TEMPLATE, resolveOutputPath } from "./features/hdr-convert/output-naming";
import { deriveShiftedPath, shiftSubtitles } from "./features/timing-shift/timing-engine";
import { parseSubtitle } from "./lib/subtitle-parser";

export interface HdrConversionRequest {
  inputPath: string;
  content: string;
  eotf: Eotf;
  brightness?: number;
  outputTemplate?: string;
}

export interface HdrConversionResult {
  outputPath: string;
  content: string;
}

export interface ShiftConversionRequest {
  inputPath: string;
  content: string;
  offsetMs: number;
  thresholdMs?: number;
  outputTemplate?: string;
}

export interface ShiftConversionResult {
  outputPath: string;
  content: string;
  format: string;
  captionCount: number;
  shiftedCount: number;
}

export function convertHdr(request: HdrConversionRequest): HdrConversionResult {
  const brightness = request.brightness ?? DEFAULT_BRIGHTNESS;
  const outputTemplate = request.outputTemplate ?? DEFAULT_TEMPLATE;
  const outputPath = resolveOutputPath(request.inputPath, outputTemplate, request.eotf);
  const fileName = request.inputPath.replace(/\\/g, "/").split("/").pop() ?? request.inputPath;

  if (isNativeAss(fileName)) {
    return {
      outputPath,
      content: processAssContent(request.content, brightness, request.eotf),
    };
  }

  if (isConvertible(fileName)) {
    const preprocessed = processSrtUserText(request.content);
    const { captions } = parseSubtitle(preprocessed, DEFAULT_STYLE.fps);
    const rawAss = buildAssDocument(
      captions.map((caption) => ({
        start: caption.start,
        end: caption.end,
        text: caption.text,
      })),
      DEFAULT_STYLE
    );
    return {
      outputPath,
      content: processAssContent(rawAss, brightness, request.eotf),
    };
  }

  throw new Error(`Unsupported subtitle format: ${fileName}`);
}

export function convertShift(request: ShiftConversionRequest): ShiftConversionResult {
  const result = shiftSubtitles(request.content, {
    offsetMs: request.offsetMs,
    thresholdMs: request.thresholdMs,
  });

  return {
    outputPath: resolveShiftOutputPath(request.inputPath, request.outputTemplate, result.format),
    content: result.content,
    format: result.format,
    captionCount: result.captionCount,
    shiftedCount: result.preview.filter((entry) => entry.wasShifted).length,
  };
}

function resolveShiftOutputPath(
  inputPath: string,
  template: string | undefined,
  format: string
): string {
  if (!template) {
    return deriveShiftedPath(inputPath);
  }

  const usedBackslash = inputPath.includes("\\");
  const normalized = inputPath.replace(/\\/g, "/");
  const lastSlash = normalized.lastIndexOf("/");
  const dir = lastSlash >= 0 ? normalized.slice(0, lastSlash) : "";
  const fullName = lastSlash >= 0 ? normalized.slice(lastSlash + 1) : normalized;
  const lastDot = fullName.lastIndexOf(".");
  const ext = lastDot > 0 ? fullName.slice(lastDot) : "";
  let baseName = lastDot > 0 ? fullName.slice(0, lastDot) : fullName;

  if (!dir || !isAbsoluteInputPath(inputPath)) {
    throw new Error("Input path must be absolute");
  }
  if (baseName.toLowerCase().endsWith(".shifted")) {
    baseName = baseName.slice(0, -".shifted".length);
  }
  if (!baseName || !baseName.replace(/^\.+/, "").trim()) {
    throw new Error("Input filename has no valid stem");
  }

  const outputName = template
    .replace(/\{name\}/g, baseName)
    .replace(/\{ext\}/g, ext)
    .replace(/\{format\}/g, format.toLowerCase())
    .replace(/\.{2,}/g, ".");

  if (!outputName.trim()) {
    throw new Error("Template resolves to empty filename");
  }
  if (/[\x00-\x1f\x7f<>:"|?*\\/]/.test(outputName)) {
    throw new Error(`Output filename contains illegal characters: ${outputName}`);
  }

  const outputPath = `${dir}/${outputName}`;
  if (outputPath.toLowerCase() === normalized.toLowerCase()) {
    throw new Error("Output path is the same as input (would overwrite source file)");
  }
  return usedBackslash ? outputPath.replace(/\//g, "\\") : outputPath;
}

function isAbsoluteInputPath(path: string): boolean {
  return path.startsWith("/") || path.startsWith("\\") || /^[A-Za-z]:[\\/]/.test(path);
}

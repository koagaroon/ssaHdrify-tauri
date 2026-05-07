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

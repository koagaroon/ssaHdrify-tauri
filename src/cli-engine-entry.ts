import { parse as parseAss } from "ass-compiler";
import { processAssContent } from "./features/hdr-convert/ass-processor";
import { SECTION_HEADER_RE } from "./features/hdr-convert/ass-processor";
import {
  DEFAULT_STYLE,
  buildAssDocument,
  isConvertible,
  isNativeAss,
  processSrtUserText,
} from "./features/hdr-convert/srt-converter";
import { DEFAULT_BRIGHTNESS, type Eotf } from "./features/hdr-convert/color-engine";
import { DEFAULT_TEMPLATE, resolveOutputPath } from "./features/hdr-convert/output-naming";
import {
  buildPairings,
  deriveRenameOutputPath,
  isNoOpRename,
  parseFilename,
  type OutputMode,
  type PairingSource,
  type ParsedFile,
} from "./features/batch-rename/pairing-engine";
import { buildFontEntry } from "./features/font-embed/ass-uuencode";
import {
  collectFontsWithParser,
  fontKeyLabel,
  type FontKey,
} from "./features/font-embed/font-collector";
import { deriveShiftedPath, shiftSubtitles } from "./features/timing-shift/timing-engine";
import { extractLangFromBaseName, LANG_TAGS } from "./lib/lang-detection";
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

export interface RenamePlanRequest {
  paths: string[];
  mode: OutputMode;
  outputDir?: string | null;
  langs?: string;
}

export interface RenamePlanResult {
  videoCount: number;
  subtitleCount: number;
  unknownCount: number;
  ignoredCount: number;
  pairings: RenamePlanRow[];
}

export interface RenamePlanRow {
  inputPath: string;
  outputPath: string;
  videoPath: string;
  source: PairingSource;
  key: string;
  language: string;
  noOp: boolean;
}

export interface FontEmbedPlanRequest {
  inputPath: string;
  content: string;
  outputTemplate?: string;
}

export interface FontEmbedPlanResult {
  outputPath: string;
  fonts: FontEmbedUsage[];
}

export interface FontEmbedUsage {
  family: string;
  bold: boolean;
  italic: boolean;
  label: string;
  fontName: string;
  glyphCount: number;
  codepoints: number[];
}

export interface FontEmbedApplyRequest {
  content: string;
  fonts: FontSubsetPayload[];
}

export interface FontSubsetPayload {
  fontName: string;
  data: number[];
}

export interface FontEmbedApplyResult {
  content: string;
  embeddedCount: number;
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

export function planRename(request: RenamePlanRequest): RenamePlanResult {
  const selection = parseLanguageSelection(request.langs ?? "auto");
  const categorized = categorizeRenamePaths(request.paths);
  const videos = categorized.videos.map((path) => parseFilename(path, fileNameFromPath(path)));
  const subtitles = categorized.subtitles.map((path) =>
    parseFilename(path, fileNameFromPath(path))
  );
  const filteredSubtitles = filterSubtitlesForLanguages(subtitles, selection);
  const candidates =
    selection.kind === "auto"
      ? buildAutoRenameCandidates(videos, filteredSubtitles)
      : buildMultiLanguageRenameCandidates(videos, filteredSubtitles);

  return {
    videoCount: videos.length,
    subtitleCount: subtitles.length,
    unknownCount: categorized.unknown.length,
    ignoredCount: categorized.ignored.length,
    pairings: candidates.map((candidate) => {
      const outputPath = deriveRenameOutputPath(
        candidate.video.path,
        candidate.subtitle.path,
        request.mode,
        request.outputDir ?? null
      );
      return {
        inputPath: candidate.subtitle.path,
        outputPath,
        videoPath: candidate.video.path,
        source: candidate.source,
        key: candidate.key,
        language: subtitleLanguage(candidate.subtitle.name),
        noOp: isNoOpRename(candidate.subtitle.path, outputPath),
      };
    }),
  };
}

export function planFontEmbed(request: FontEmbedPlanRequest): FontEmbedPlanResult {
  const usages = collectFontsWithParser(request.content, parseAss);

  return {
    outputPath: resolveEmbedOutputPath(request.inputPath, request.outputTemplate),
    fonts: usages.map((usage) => ({
      family: usage.key.family,
      bold: usage.key.bold,
      italic: usage.key.italic,
      label: fontKeyLabel(usage.key),
      fontName: buildFontFileName(usage.key),
      glyphCount: usage.codepoints.size,
      codepoints: Array.from(usage.codepoints),
    })),
  };
}

export function applyFontEmbed(request: FontEmbedApplyRequest): FontEmbedApplyResult {
  const fontEntries = request.fonts.map((font) =>
    buildFontEntry(font.fontName, Uint8Array.from(font.data))
  );

  if (fontEntries.length === 0) {
    return {
      content: request.content,
      embeddedCount: 0,
    };
  }

  const fontsSection = `[Fonts]\n${fontEntries.join("\n\n")}\n`;
  return {
    content: insertFontsSection(request.content, fontsSection),
    embeddedCount: fontEntries.length,
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
  // eslint-disable-next-line no-control-regex -- reject control characters in output filenames
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

function resolveEmbedOutputPath(inputPath: string, template = "{name}.embed.ass"): string {
  const usedBackslash = inputPath.includes("\\");
  const normalized = inputPath.replace(/\\/g, "/");
  const lastSlash = normalized.lastIndexOf("/");
  const dir = lastSlash >= 0 ? normalized.slice(0, lastSlash) : "";
  const fullName = lastSlash >= 0 ? normalized.slice(lastSlash + 1) : normalized;
  const lastDot = fullName.lastIndexOf(".");
  const baseName = lastDot > 0 ? fullName.slice(0, lastDot) : fullName;

  if (!dir || !isAbsoluteInputPath(inputPath)) {
    throw new Error("Input path must be absolute");
  }
  if (!baseName || !baseName.replace(/^\.+/, "").trim()) {
    throw new Error("Input filename has no valid stem");
  }

  const outputName = template
    .replace(/\{name\}/g, baseName)
    .replace(/\{ext\}/g, ".ass")
    .replace(/\.{2,}/g, ".");

  if (!outputName.trim()) {
    throw new Error("Template resolves to empty filename");
  }
  // eslint-disable-next-line no-control-regex -- reject control characters in output filenames
  if (/[\x00-\x1f\x7f<>:"|?*\\/]/.test(outputName)) {
    throw new Error(`Output filename contains illegal characters: ${outputName}`);
  }

  const outputPath = `${dir}/${outputName}`;
  if (outputPath.toLowerCase() === normalized.toLowerCase()) {
    throw new Error("Output path is the same as input (would overwrite source file)");
  }
  return usedBackslash ? outputPath.replace(/\//g, "\\") : outputPath;
}

function buildFontFileName(key: FontKey): string {
  let name = key.family
    .toLowerCase()
    .replace(/[^a-z0-9_-]/g, "_")
    .replace(/_+/g, "_")
    .replace(/^_|_$/g, "");
  if (!name) name = `font_${familyFnvHash(key.family)}`;
  if (key.bold) name += "_bold";
  if (key.italic) name += "_italic";
  return `${name}.ttf`;
}

function familyFnvHash(family: string): string {
  let h = 0x811c9dc5;
  for (const ch of family) {
    const cp = ch.codePointAt(0) ?? 0;
    h ^= cp & 0xff;
    h = Math.imul(h, 0x01000193);
    h ^= (cp >>> 8) & 0xff;
    h = Math.imul(h, 0x01000193);
    h ^= (cp >>> 16) & 0xff;
    h = Math.imul(h, 0x01000193);
  }
  return (h >>> 0).toString(16).padStart(8, "0");
}

function insertFontsSection(content: string, fontsSection: string): string {
  const lineEnding = content.includes("\r\n") ? "\r\n" : "\n";
  const normalized = content.replace(/[\u2028\u2029]/g, "\n");
  const lines = normalized.split(/\r?\n/);
  const adaptedFontsSection = fontsSection.replace(/\n/g, lineEnding);
  const headerFontsRe = /^\[[Ff][Oo][Nn][Tt][Ss]\][ \t]*$/;
  const existingFontsIdx = lines.findIndex((line) => headerFontsRe.test(line));

  const buildBefore = (endIdx: number): { text: string; sep: string } => {
    const slice = lines.slice(0, endIdx);
    while (slice.length > 0 && slice[slice.length - 1].trim() === "") {
      slice.pop();
    }
    const text = slice.join(lineEnding);
    const sep = slice.length > 0 ? lineEnding + lineEnding : "";
    return { text, sep };
  };

  const buildAfter = (startIdx: number): string => {
    const slice = lines.slice(startIdx);
    while (slice.length > 0 && slice[0].trim() === "") {
      slice.shift();
    }
    return slice.join(lineEnding);
  };

  const isSectionHeader = (line: string) => SECTION_HEADER_RE.test(line.trim().toLowerCase());

  if (existingFontsIdx >= 0) {
    let endIdx = existingFontsIdx + 1;
    while (endIdx < lines.length && !isSectionHeader(lines[endIdx])) {
      endIdx += 1;
    }

    const { text: before, sep } = buildBefore(existingFontsIdx);
    const after = buildAfter(endIdx);
    const afterSep = after.length > 0 ? lineEnding : "";
    return `${before}${sep}${adaptedFontsSection}${afterSep}${after}`;
  }

  const headerEventsRe = /^\[[Ee][Vv][Ee][Nn][Tt][Ss]\][ \t]*$/;
  const eventsIdx = lines.findIndex((line) => headerEventsRe.test(line));
  if (eventsIdx >= 0) {
    const { text: before, sep } = buildBefore(eventsIdx);
    const after = lines.slice(eventsIdx).join(lineEnding);
    return `${before}${sep}${adaptedFontsSection}${lineEnding}${after}`;
  }

  const trimmedContent = content.replace(/(\r\n|\n)+$/, "");
  return `${trimmedContent}${lineEnding}${lineEnding}${adaptedFontsSection}`;
}

const VIDEO_EXTS = new Set([
  "mp4",
  "mkv",
  "avi",
  "mov",
  "ts",
  "m2ts",
  "webm",
  "flv",
  "wmv",
  "mpg",
  "mpeg",
  "m4v",
]);
const SUBTITLE_EXTS = new Set(["ass", "ssa", "srt", "sub", "vtt", "sbv", "lrc"]);
const IGNORED_EXTS = new Set([
  "torrent",
  "zip",
  "rar",
  "7z",
  "tar",
  "gz",
  "bz2",
  "xz",
  "tgz",
  "mka",
  "flac",
  "mp3",
  "m4a",
  "aac",
]);

type RenameCategory = "video" | "subtitle" | "ignored" | "unknown";
type LanguageSelection =
  | { kind: "auto" }
  | { kind: "all" }
  | { kind: "list"; languages: Set<string> };

interface CategorizedRenamePaths {
  videos: string[];
  subtitles: string[];
  ignored: string[];
  unknown: string[];
}

interface RenameCandidate {
  video: ParsedFile;
  subtitle: ParsedFile;
  source: PairingSource;
  key: string;
}

function categorizeRenamePaths(paths: string[]): CategorizedRenamePaths {
  const videos: string[] = [];
  const subtitles: string[] = [];
  const ignored: string[] = [];
  const unknown: string[] = [];

  for (const path of paths) {
    const category = categorizeRenamePath(path);
    if (category === "video") videos.push(path);
    else if (category === "subtitle") subtitles.push(path);
    else if (category === "ignored") ignored.push(path);
    else unknown.push(path);
  }

  return { videos, subtitles, ignored, unknown };
}

function categorizeRenamePath(path: string): RenameCategory {
  const name = fileNameFromPath(path);
  const dot = name.lastIndexOf(".");
  if (dot < 0) return "unknown";
  const ext = name.slice(dot + 1).toLowerCase();
  if (VIDEO_EXTS.has(ext)) return "video";
  if (SUBTITLE_EXTS.has(ext)) return "subtitle";
  if (IGNORED_EXTS.has(ext)) return "ignored";
  return "unknown";
}

function parseLanguageSelection(raw: string): LanguageSelection {
  const normalized = raw.trim().toLowerCase();
  if (!normalized || normalized === "auto") return { kind: "auto" };
  if (normalized === "all") return { kind: "all" };

  const languages = new Set<string>();
  for (const part of normalized.split(",")) {
    const token = part.trim();
    if (!token) continue;
    if (!LANG_TAGS.has(token)) {
      throw new Error(`Unsupported language code: ${token}`);
    }
    languages.add(canonicalLanguage(token));
  }

  if (languages.size === 0) {
    throw new Error("Language list is empty");
  }
  return { kind: "list", languages };
}

function filterSubtitlesForLanguages(
  subtitles: ParsedFile[],
  selection: LanguageSelection
): ParsedFile[] {
  if (selection.kind !== "list") return subtitles;
  return subtitles.filter((subtitle) => selection.languages.has(subtitleLanguage(subtitle.name)));
}

function buildAutoRenameCandidates(
  videos: ParsedFile[],
  subtitles: ParsedFile[]
): RenameCandidate[] {
  return buildPairings(videos, subtitles)
    .filter((row) => row.selected && row.video && row.subtitle)
    .map((row) => ({
      video: parseFilename(row.video!.path, row.video!.name),
      subtitle: parseFilename(row.subtitle!.path, row.subtitle!.name),
      source: row.source,
      key: row.key,
    }));
}

function buildMultiLanguageRenameCandidates(
  videos: ParsedFile[],
  subtitles: ParsedFile[]
): RenameCandidate[] {
  const videosByKey = groupMatchedFilesByKey(videos);
  const subtitlesByKey = groupMatchedFilesByKey(subtitles);
  const candidates: RenameCandidate[] = [];

  for (const key of Array.from(videosByKey.keys()).sort(comparePairingKeys)) {
    const keyVideos = videosByKey.get(key) ?? [];
    const keySubtitles = subtitlesByKey.get(key) ?? [];
    if (keySubtitles.length === 0) continue;

    if (keyVideos.length === 1) {
      for (const subtitle of keySubtitles) {
        candidates.push({
          video: keyVideos[0],
          subtitle,
          source: "regex",
          key,
        });
      }
      continue;
    }

    for (let i = 0; i < keyVideos.length; i += 1) {
      const subtitle = keySubtitles[i];
      if (!subtitle) continue;
      candidates.push({
        video: keyVideos[i],
        subtitle,
        source: "warning",
        key,
      });
    }
  }

  return candidates;
}

function groupMatchedFilesByKey(files: ParsedFile[]): Map<string, ParsedFile[]> {
  const groups = new Map<string, ParsedFile[]>();
  for (const file of files) {
    if (file.episode === null) continue;
    const key = `${file.season}|${file.episode}`;
    const group = groups.get(key) ?? [];
    group.push(file);
    groups.set(key, group);
  }
  return groups;
}

function comparePairingKeys(a: string, b: string): number {
  const [as, ae] = a.split("|").map((n) => parseInt(n, 10) || 0);
  const [bs, be] = b.split("|").map((n) => parseInt(n, 10) || 0);
  if (as !== bs) return as - bs;
  return ae - be;
}

function subtitleLanguage(name: string): string {
  const dot = name.lastIndexOf(".");
  const baseName = dot > 0 ? name.slice(0, dot) : name;
  return canonicalLanguage(extractLangFromBaseName(baseName));
}

function canonicalLanguage(language: string): string {
  switch (language.toLowerCase()) {
    case "chs":
    case "sc":
    case "zh":
    case "zh-cn":
      return "sc";
    case "cht":
    case "tc":
    case "zh-tw":
      return "tc";
    case "ja":
    case "jpn":
    case "jp":
      return "jp";
    case "eng":
      return "en";
    default:
      return language.toLowerCase();
  }
}

function fileNameFromPath(path: string): string {
  const normalized = path.replace(/\\/g, "/");
  const slash = normalized.lastIndexOf("/");
  return slash >= 0 ? normalized.slice(slash + 1) : normalized;
}

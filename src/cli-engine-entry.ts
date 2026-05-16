import { parse as parseAss } from "ass-compiler";
import { isWindowsRuntime } from "./lib/platform";
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
import { buildFontFileName, insertFontsSection } from "./features/font-embed/font-embedder";
import { collectFontsWithParser, fontKeyLabel } from "./features/font-embed/font-collector";
import { deriveShiftedPath, shiftSubtitles } from "./features/timing-shift/timing-engine";
import { extractLangFromBaseName, LANG_TAGS } from "./lib/lang-detection";
import {
  assertSafeOutputFilename,
  assertSafeOutputPath,
  decomposeInputPath,
  substituteTemplate,
} from "./lib/path-validation";
import { parseSubtitle } from "./lib/subtitle-parser";
import { stripUnicodeControls } from "./lib/unicode-controls";

// Chain feature — runtime + types re-exported so the Rust shell can
// reach them via the bundled engine.js. Adding the chain entry here
// is what makes the chain runtime get included in esbuild's bundle.
export { runChain, resolveChainOutputPath } from "./features/chain/chain-runtime";
export type {
  ChainPlan,
  ChainResult,
  ChainStep,
  StepKind,
  HdrStepParams,
  ShiftStepParams,
  EmbedStepParams,
} from "./features/chain/chain-types";

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

/**
 * Standalone-embed font subset payload: bytes serialized as JSON
 * `number[]`. Distinct from chain-types.ts's `ChainFontSubsetPayload`
 * which uses base64 (`dataB64`); the two coexist intentionally on
 * different IPC paths. Don't auto-import; pick the one matching your
 * call site.
 */
export interface FontSubsetPayload {
  fontName: string;
  data: number[];
}

export interface FontEmbedApplyResult {
  content: string;
  embeddedCount: number;
}

export function resolveHdrOutputPath(request: {
  inputPath: string;
  eotf: Eotf;
  outputTemplate?: string;
}): string {
  // Cheap path-only resolution. MUST stay byte-identical to convertHdr's
  // returned outputPath — both route through resolveOutputPath with the
  // same template defaulting, so byte equality holds by construction.
  const outputTemplate = request.outputTemplate ?? DEFAULT_TEMPLATE;
  return resolveOutputPath(request.inputPath, outputTemplate, request.eotf);
}

export function convertHdr(request: HdrConversionRequest): HdrConversionResult {
  const brightness = request.brightness ?? DEFAULT_BRIGHTNESS;
  const outputTemplate = request.outputTemplate ?? DEFAULT_TEMPLATE;
  // outputPath is computed here AND in resolveHdrOutputPath; the CLI
  // shell calls the cheap resolver first for dedup/exists checks, then
  // calls convertHdr only for content. The duplicate compute is cheap
  // (string ops only) and the two paths route through resolveOutputPath
  // with identical defaults — byte equality is structurally guaranteed.
  const outputPath = resolveOutputPath(request.inputPath, outputTemplate, request.eotf);
  // Backslash → forward only on Windows. On POSIX, `\` is a valid
  // filename character; treating it as a separator would mis-extract
  // `EP\01.ass` as `01.ass` instead of the actual filename.
  const normalizedForName = isWindowsRuntime
    ? request.inputPath.replace(/\\/g, "/")
    : request.inputPath;
  const fileName = normalizedForName.split("/").pop() ?? request.inputPath;

  if (isNativeAss(fileName)) {
    return {
      outputPath,
      content: processAssContent(request.content, brightness, request.eotf),
    };
  }

  if (isConvertible(fileName)) {
    const preprocessed = processSrtUserText(request.content);
    const { captions } = parseSubtitle(preprocessed, DEFAULT_STYLE.fps);
    // Drop oversized-text placeholders before building ASS. parseSrt /
    // parseVtt / parseSub / parseAss emit `{ text: "", skipped: true }`
    // for captions over MAX_CAPTION_TEXT_LEN (W11.1 contract); without
    // this filter the CLI HDR path serializes each as a blank Dialogue
    // line. Mirrors HdrConvert.tsx:444 GUI-side filter.
    const rawAss = buildAssDocument(
      captions
        .filter((caption) => !caption.skipped)
        .map((caption) => ({
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
    outputPath: resolveShiftOutputPathInternal(
      request.inputPath,
      request.outputTemplate,
      result.format
    ),
    content: result.content,
    format: result.format,
    captionCount: result.captionCount,
    shiftedCount: result.preview.filter((entry) => entry.wasShifted).length,
  };
}

// Note: planRename is intentionally atomic — a single bad output
// filename in any pairing row throws synchronously and aborts the
// whole plan, unlike HDR/Shift/Embed's per-file failure-and-continue
// model. Rationale: the rename plan is one logical unit (the user
// reviews the entire pairing grid before any writes happen), so a
// validator failure on any row signals "the plan needs to be
// recomputed", not "skip this file and continue with the rest."
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
    outputPath: resolveEmbedOutputPathInternal(request.inputPath, request.outputTemplate),
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

/**
 * Cheap path-only resolver for shift, used by the CLI shell to dedup
 * outputs and skip-on-exists BEFORE invoking the heavy convert_shift.
 * Caller MUST ensure the template does NOT contain `{format}` — that
 * token requires content parsing (the format value comes from
 * shiftSubtitles' parser output), which the cheap path cannot provide.
 * The Rust shell pre-checks `args.output_template.contains("{format}")`
 * and falls back to heavy-first ordering when present.
 *
 * Output is byte-identical to convertShift's returned outputPath for
 * templates without {format}; pinned in cli-engine-roundtrip.test.ts.
 */
export function resolveShiftOutputPath(request: {
  inputPath: string;
  outputTemplate?: string;
}): string {
  // Empty placeholder for `format` is safe: with no `{format}` token
  // in the template, the substitution is a no-op.
  return resolveShiftOutputPathInternal(request.inputPath, request.outputTemplate, "");
}

function resolveShiftOutputPathInternal(
  inputPath: string,
  template: string | undefined,
  format: string
): string {
  if (!template) {
    return deriveShiftedPath(inputPath);
  }

  const parts = decomposeInputPath(inputPath);
  const { dir, ext, normalized, usedBackslash } = parts;
  let { baseName } = parts;
  if (baseName.toLowerCase().endsWith(".shifted")) {
    baseName = baseName.slice(0, -".shifted".length);
  }
  // Re-check valid stem AFTER `.shifted` strip — `EP01.shifted.ass` is
  // legitimate input but strips to `EP01` (fine), while `.shifted.ass`
  // strips to "" (must reject).
  if (!baseName || !baseName.replace(/^\.+/, "").trim()) {
    throw new Error("Input filename has no valid stem");
  }

  const outputName = substituteTemplate(template, {
    name: baseName,
    ext,
    format: format.toLowerCase(),
  });

  // Shared filename + path safety checks (reserved names, traversal,
  // MAX_PATH, self-overwrite). Same helpers used by HDR's resolver and
  // by GUI's deriveShiftedPath / deriveEmbeddedPath.
  assertSafeOutputFilename(outputName);
  const outputPath = `${dir}/${outputName}`;
  assertSafeOutputPath(outputPath, normalized);

  return usedBackslash ? outputPath.replace(/\//g, "\\") : outputPath;
}

/**
 * Cheap path-only resolver for embed, used by the CLI shell to dedup
 * outputs and skip-on-exists BEFORE planFontEmbed parses the ASS.
 * No format dependency, so always works.
 *
 * Output is byte-identical to what planFontEmbed would have returned;
 * pinned in cli-engine-roundtrip.test.ts.
 */
export function resolveEmbedOutputPath(request: {
  inputPath: string;
  outputTemplate?: string;
}): string {
  return resolveEmbedOutputPathInternal(request.inputPath, request.outputTemplate);
}

function resolveEmbedOutputPathInternal(inputPath: string, template = "{name}.embed.ass"): string {
  const { dir, baseName, normalized, usedBackslash } = decomposeInputPath(inputPath);

  const outputName = substituteTemplate(template, { name: baseName, ext: ".ass" });

  // Shared filename + path safety checks. See note in
  // resolveShiftOutputPathInternal.
  assertSafeOutputFilename(outputName);
  const outputPath = `${dir}/${outputName}`;
  assertSafeOutputPath(outputPath, normalized);

  return usedBackslash ? outputPath.replace(/\//g, "\\") : outputPath;
}

// buildFontFileName / familyStableHash / insertFontsSection are
// re-exported from font-embedder so the GUI and CLI paths share one
// canonical implementation. Previously they were duplicated verbatim
// here; the duplicates have been removed (N-R5-FECHAIN-12 /
// N-R5-FECHAIN-13).

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
    // Round 8 N-R8-N1-4: `ko` / `kor` reconcile. Without this branch,
    // a CLI invocation with `--langs kor` could not match subtitle
    // filenames carrying the `.ko.ass` suffix (and vice versa),
    // silently filtering the row. Mirrors the sc / tc / jp / en
    // reconciliation already in place.
    case "ko":
    case "kor":
      return "ko";
    default:
      return language.toLowerCase();
  }
}

function fileNameFromPath(path: string): string {
  // Conditional separator normalization: `\` is a valid filename char on
  // POSIX (Codex edb0e74f / 8850ede7). Unconditional conversion turned
  // single Linux filenames containing `\` into path components. Round 8
  // A-R8-N4-12 gated `tauri-api.ts::fileNameFromPath` on `isWindowsRuntime`
  // too, so the two siblings now share the same separator semantics.
  //
  // Round 7 Wave 7.1: matches the BiDi scrubbing the tauri-api.ts
  // sibling does via stripUnicodeControls.
  //
  // Round 11 W11.2 (M3 / N3-R11-01): also strip ASCII C0 + DEL + C1
  // (`\x00-\x1f\x7f-\x9f`) — parity with Round 10 N-R10-025 in
  // tauri-api.ts. stripUnicodeControls only covers BiDi / zero-width;
  // ASCII control bytes (\0, \t, \r, \n, ESC) previously passed through
  // CLI-side fileNameFromPath into stderr log lines and JSON output.
  // The CLI engine's input paths flow from clap's argv into
  // chain/embed/hdr/shift filename derivation, then back through this
  // helper for display — without the strip, a crafted argv would still
  // break log row formatting on the CLI side even after the GUI side
  // closed the same gap.
  const normalized = isWindowsRuntime ? path.replace(/\\/g, "/") : path;
  const slash = normalized.lastIndexOf("/");
  const raw = slash >= 0 ? normalized.slice(slash + 1) : normalized;
  // eslint-disable-next-line no-control-regex -- intentional: scrub C0 / DEL / C1
  return stripUnicodeControls(raw).replace(/[\x00-\x1f\x7f-\x9f]/g, "");
}

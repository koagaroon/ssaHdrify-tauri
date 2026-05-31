import { parse as parseAss } from "ass-compiler";
import { processAssContent } from "./features/hdr-convert/ass-processor";
import {
  DEFAULT_STYLE,
  buildAssDocumentFromCaptions,
  isConvertible,
  isNativeAss,
  processSrtUserText,
} from "./features/hdr-convert/srt-converter";
import { DEFAULT_BRIGHTNESS, type Eotf } from "./features/hdr-convert/color-engine";
import { DEFAULT_TEMPLATE, resolveOutputPath } from "./features/hdr-convert/output-naming";
import {
  buildPairings,
  compareKeys,
  deriveRenameOutputPath,
  isNoOpRename,
  parseFilename,
  type OutputMode,
  type PairingSource,
  type ParsedFile,
} from "./features/batch-rename/pairing-engine";
import { buildFontEntry } from "./features/font-embed/ass-uuencode";
import { assertAssShape, insertFontsSection } from "./features/font-embed/ass-font-section";
import { buildFontFileName } from "./features/font-embed/font-embedder";
import { collectFontsWithParser, fontKeyLabel } from "./features/font-embed/font-collector";
import {
  assertFiniteShiftMs,
  deriveShiftedPath,
  shiftSubtitles,
} from "./features/timing-shift/timing-engine";
import { extractLangFromBaseName, LANG_TAGS } from "./lib/lang-detection";
import {
  assertSafeOutputFilename,
  assertSafeOutputPath,
  decomposeInputPath,
  fileNameFromPath,
  substituteTemplate,
} from "./lib/path-validation";
import { categorize, type RenameCategory } from "./lib/rename-extensions";
import { parseSubtitle } from "./lib/subtitle-parser";
import { sanitizeError } from "./lib/dedup-helpers";
import { stripUnicodeControls } from "./lib/unicode-controls";
import { decodeBase64Bytes } from "./lib/base64-bytes";

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
  /**
   * Count of captions whose text exceeded MAX_CAPTION_TEXT_LEN (64 KB)
   * and were emitted as skipped placeholders by parseSubtitle. The CLI
   * shell surfaces this via stderr (no-silent-action) to mirror the
   * GUI's `msg_oversized_skipped` warning. Always
   * 0 for native-ASS input (processAssContent is line-based, doesn't
   * route through parseSubtitle; the MAX_CAPTION_TEXT_LEN concept
   * doesn't apply at this layer).
   */
  skippedCount: number;
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
  /**
   * Count of captions whose text exceeded MAX_CAPTION_TEXT_LEN (64 KB)
   * and were emitted as skipped placeholders. Forwarded from
   * `shiftSubtitles`'s ShiftResult; the CLI shell stderr-surfaces this
   * to mirror TimingShift.tsx's msg_oversized_skipped.
   */
  skippedCount: number;
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

export interface FontDiagnosticsPlanRequest {
  content: string;
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
 * Standalone-embed font subset payload: bytes serialized as base64.
 * This intentionally matches chain-types.ts's `ChainFontSubsetPayload`
 * so large CJK font batches do not expand into JSON number arrays
 * before reaching V8.
 */
export interface FontSubsetPayload {
  fontName: string;
  dataB64: string;
}

export interface FontEmbedApplyResult {
  content: string;
  embeddedCount: number;
}

// The CLI engine entry is a real input boundary: `eotf` arrives as a JSON
// string from the deno_core op layer, where the `Eotf` TS type is NOT enforced
// at runtime. A bogus value would flow into the {eotf} filename token as
// wrong-but-legal output (sRgbToHdr silently treats any non-"HLG" value as
// "PQ"), so reject it up front rather than emit a mislabeled file.
const VALID_EOTFS: readonly Eotf[] = ["PQ", "HLG"];
function assertValidEotf(eotf: Eotf): void {
  if (!VALID_EOTFS.includes(eotf)) {
    throw new Error(`Invalid eotf '${String(eotf)}': expected one of ${VALID_EOTFS.join(", ")}`);
  }
}

export function resolveHdrOutputPath(request: {
  inputPath: string;
  eotf: Eotf;
  outputTemplate?: string;
}): string {
  assertValidEotf(request.eotf);
  // Cheap path-only resolution. MUST stay byte-identical to convertHdr's
  // returned outputPath — both route through resolveOutputPath with the
  // same template defaulting, so byte equality holds by construction.
  const outputTemplate = request.outputTemplate ?? DEFAULT_TEMPLATE;
  return resolveOutputPath(request.inputPath, outputTemplate, request.eotf);
}

export function convertHdr(request: HdrConversionRequest): HdrConversionResult {
  assertValidEotf(request.eotf);
  const brightness = request.brightness ?? DEFAULT_BRIGHTNESS;
  const outputTemplate = request.outputTemplate ?? DEFAULT_TEMPLATE;
  // outputPath is computed here AND in resolveHdrOutputPath; the CLI
  // shell calls the cheap resolver first for dedup/exists checks, then
  // calls convertHdr only for content. The duplicate compute is cheap
  // (string ops only) and the two paths route through resolveOutputPath
  // with identical defaults — byte equality is structurally guaranteed.
  const outputPath = resolveOutputPath(request.inputPath, outputTemplate, request.eotf);
  // Filename extraction for the extension-based dispatch below. R2
  // W1: this used to inline the windows-separator + split logic; now
  // routed through the shared `fileNameFromPath` so the empty-string
  // fallback + control-char strip stay in lockstep with the GUI side.
  const fileName = fileNameFromPath(request.inputPath);

  if (isNativeAss(fileName)) {
    return {
      outputPath,
      content: processAssContent(request.content, brightness, request.eotf),
      skippedCount: 0,
    };
  }

  if (isConvertible(fileName)) {
    const preprocessed = processSrtUserText(request.content);
    const { captions } = parseSubtitle(preprocessed, DEFAULT_STYLE.fps);
    // Drop oversized-text placeholders before building ASS. parseSrt /
    // parseSub / parseVtt emit `{ text: "", skipped: true }` for captions over
    // MAX_CAPTION_TEXT_LEN (W11.1 contract); without this filter the
    // CLI HDR path serializes each as a blank Dialogue line. Mirrors
    // HdrConvert.tsx GUI-side filter. (parseAss placeholders don't
    // reach here — `.ass` goes through the isNativeAss branch above.)
    const { content: rawAss, skippedCount } = buildAssDocumentFromCaptions(captions, DEFAULT_STYLE);
    return {
      outputPath,
      content: processAssContent(rawAss, brightness, request.eotf),
      skippedCount,
    };
  }

  throw new Error(`Unsupported subtitle format: ${fileName}`);
}

export function convertShift(request: ShiftConversionRequest): ShiftConversionResult {
  assertFiniteShiftMs(request.offsetMs, request.thresholdMs);
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
    skippedCount: result.skippedCount,
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
  assertAssShape(request.content);
  const usages = collectFontsWithParser(request.content, parseAss);

  return {
    outputPath: resolveEmbedOutputPathInternal(request.inputPath, request.outputTemplate),
    fonts: toFontEmbedUsages(usages),
  };
}

export function planFontDiagnostics(
  request: FontDiagnosticsPlanRequest
): Pick<FontEmbedPlanResult, "fonts"> {
  assertAssShape(request.content);
  const usages = collectFontsWithParser(request.content, parseAss);

  return {
    fonts: toFontEmbedUsages(usages),
  };
}

function toFontEmbedUsages(usages: ReturnType<typeof collectFontsWithParser>): FontEmbedUsage[] {
  return usages.map((usage) => ({
    family: usage.key.family,
    bold: usage.key.bold,
    italic: usage.key.italic,
    label: fontKeyLabel(usage.key),
    fontName: buildFontFileName(usage.key),
    glyphCount: usage.codepoints.size,
    codepoints: Array.from(usage.codepoints),
  }));
}

export function applyFontEmbed(request: FontEmbedApplyRequest): FontEmbedApplyResult {
  // Shape / size / line-count gate runs BEFORE the zero-fontEntries
  // early-return so every entry into this helper hits the same
  // backstop. `insertFontsSection` calls `assertAssShape` internally,
  // so the non-zero path is already covered; the zero-fonts path
  // previously returned content verbatim, leaving the standalone
  // embed CLI flow (process_embed_file's subset_payloads.is_empty()
  // short-circuit) without shape validation. Pattern 1 helper-
  // contract uniformity.
  assertAssShape(request.content);

  const fontEntries = request.fonts.map((font) =>
    buildFontEntry(font.fontName, decodeSubsetBase64(font.dataB64, font.fontName))
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

function decodeSubsetBase64(dataB64: string, name: string): Uint8Array {
  try {
    return decodeBase64Bytes(dataB64);
  } catch (e) {
    const message = sanitizeError(e);
    const safeName = stripUnicodeControls(name);
    throw new Error(`base64 decode failed for font subset '${safeName}': ${message}`, {
      cause: e,
    });
  }
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

// Font filename and ASS [Fonts] section helpers are imported from the
// GUI font-embed modules so GUI and CLI paths share one canonical
// implementation. Previously they were duplicated verbatim here; the
// duplicates have been removed.

// VIDEO_EXTS / SUBTITLE_EXTS / IGNORED_EXTS / RenameCategory now live in
// `src/lib/rename-extensions.ts` — shared with GUI BatchRename and the
// tauri-api picker filter. The GUI uses the
// `categorize(name)` helper; CLI keeps `categorizeRenamePath(path)` as a
// thin wrapper that runs `fileNameFromPath` first because CLI receives
// full argv paths, not bare filenames.

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
  // thin wrapper — delegate the ext-lookup chain to the
  // canonical `categorize` (which scans VIDEO_EXTS → SUBTITLE_EXTS →
  // IGNORED_EXTS in the same priority order). The chain was previously
  // open-coded here, so a future bucket addition or priority shift in
  // `rename-extensions.ts::categorize` would silently skip the CLI
  // path. `fileNameFromPath` still applies upstream to keep the
  // trailing-separator handling.
  return categorize(fileNameFromPath(path));
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

  for (const key of Array.from(videosByKey.keys()).sort(compareKeys)) {
    const keyVideos = videosByKey.get(key) ?? [];
    const keySubtitles = subtitlesByKey.get(key) ?? [];
    if (keySubtitles.length === 0) continue;

    if (keyVideos.length === 1) {
      for (const subtitle of keySubtitles) {
        candidates.push({
          video: keyVideos[0]!,
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
        video: keyVideos[i]!,
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

// `compareKeys` now exported from `pairing-engine.ts` ; the
// local sibling that lived here had no comments and could drift from the
// pairing-engine version's malformed-key `|| 0` WHY comment.

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
    // `ko` / `kor` reconcile. Without this branch,
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

// `fileNameFromPath` now lives in `path-validation.ts`.
// Previously the CLI sibling differed from the GUI version: it used
// `lastIndexOf("/") + slice` with no fallback, so a trailing-separator
// input returned an empty string while the GUI sibling returned the
// original path. The empty-string output silently dropped
// trailing-separator video paths from the rename plan. Consolidation
// closes the drift.

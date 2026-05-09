/**
 * Output naming — resolve output file paths from templates.
 *
 * Port of Python output_naming.py. Handles template variables,
 * tag stripping, and safety checks (path traversal, reserved names).
 */
import type { Eotf } from "./color-engine";
import { extractLangFromBaseName } from "../../lib/lang-detection";
import {
  assertSafeOutputFilename,
  assertSafeOutputPath,
  decomposeInputPath,
} from "../../lib/path-validation";

// ── Template Presets ──────────────────────────────────────

export const OUTPUT_PRESETS = [
  "{name}.hdr.ass",
  "{name}.{eotf}.ass",
  "{name}.hdr.{eotf}.ass",
] as const;

export const DEFAULT_TEMPLATE = OUTPUT_PRESETS[0];

/** Recognized video container extensions. Used to strip the trailing
 *  extension from a video filename when computing {video_name}. Naming
 *  conventions like `Show.S01E01.1080p` are common, so only known video
 *  extensions are stripped — never any trailing dotted segment. */
const VIDEO_EXTENSIONS = new Set([
  "mkv",
  "mp4",
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
  "ogv",
  "rmvb",
]);

/** Strip a recognized video extension from a filename, returning the stem.
 *  Conservative on purpose — names like `Show.S01E01` without an extension
 *  retain their trailing dotted segment. Path separators are tolerated so
 *  callers may pass either a bare name or a full path. */
function stripVideoExtension(fileName: string): string {
  if (!fileName) return "";
  const sepIdx = Math.max(fileName.lastIndexOf("/"), fileName.lastIndexOf("\\"));
  const tail = sepIdx >= 0 ? fileName.slice(sepIdx + 1) : fileName;
  const dotIdx = tail.lastIndexOf(".");
  if (dotIdx <= 0) return tail;
  const ext = tail.slice(dotIdx + 1).toLowerCase();
  return VIDEO_EXTENSIONS.has(ext) ? tail.slice(0, dotIdx) : tail;
}

/** Optional resolution context for tokens that depend on out-of-band data
 *  (a paired video filename, an explicit language tag). All fields default
 *  to empty when omitted. {video_name} requires `videoName`; {lang} prefers
 *  explicit `lang` over auto-extraction from the input filename. */
export interface ResolveOptions {
  /** Filename (with or without extension) of the paired video. The basename
   *  without extension is substituted for `{video_name}`. Empty when not
   *  paired (Tab 1–3 workflows). */
  videoName?: string;
  /** Explicit language tag for `{lang}`. When omitted, the resolver
   *  auto-extracts a tag from the input filename's trailing dotted segment
   *  (e.g., `EP01.zh.ass` → "zh"). */
  lang?: string;
}

/**
 * Resolve an output path from a template and input file path.
 *
 * @param inputPath - Full path to the input file
 * @param template - Output template string (e.g., "{name}.hdr.ass")
 * @param eotf - Transfer function for {eotf} variable
 * @param options - Optional context for {video_name} and {lang} tokens
 * @returns Resolved output file path
 * @throws Error if template resolves to unsafe path
 */
export function resolveOutputPath(
  inputPath: string,
  template: string,
  eotf: Eotf,
  options: ResolveOptions = {}
): string {
  // Decompose via the shared helper. Validates the path is absolute,
  // accepts drive-root files (`C:\foo.ass`), rejects drive-relative
  // (`C:foo.ass`), and catches control chars across the whole path. See
  // `decomposeInputPath` for the full accept/reject matrix.
  const parts = decomposeInputPath(inputPath);
  const { dir, normalized, usedBackslash } = parts;
  let { baseName } = parts;

  // Strip existing .hdr / .sdr tags in a single regex pass — the previous
  // while-loop version was O(n²) for pathological stacks like
  // "foo.hdr.hdr.hdr....hdr.ass" (each slice allocates). A compiled regex
  // collapses the whole tail in one pass.
  baseName = baseName.replace(/(\.(hdr|sdr))+$/i, "");

  // Re-check valid stem AFTER the .hdr / .sdr strip. Inputs like
  // `.hdr.ass` decompose to baseName=".hdr" (passes the helper's stem
  // check) but strip down to "" — must surface here.
  if (!baseName || !baseName.replace(/^\.+/, "").trim()) {
    throw new Error("Input filename has no valid stem");
  }

  // Resolve token values once. {lang} prefers the explicit option; falling
  // back to filename extraction lets simple Tab 1–3 workflows benefit when
  // their input is already tagged (`EP01.zh.srt`). `videoName` is purely
  // pair-driven (Tab 4); empty otherwise.
  const langValue = (options.lang ?? extractLangFromBaseName(baseName)).toLowerCase();
  const videoStem = stripVideoExtension(options.videoName ?? "");

  // Resolve template variables in a single pass to prevent double-substitution
  // (e.g., a filename containing literal "{eotf}" being expanded by the second replace)
  const resolved = template
    .replace(/\{(name|eotf|video_name|lang)\}/g, (_, key: string) => {
      switch (key) {
        case "name":
          return baseName;
        case "eotf":
          return eotf.toLowerCase();
        case "video_name":
          return videoStem;
        case "lang":
          return langValue;
        default:
          return "";
      }
    })
    // Collapse adjacent-dot artifacts produced when an optional token
    // ({lang} or {video_name}) resolves to an empty string in the middle
    // of a template like `{video_name}.{lang}.ass`. Templates without
    // empty tokens are unchanged. Side note: input filenames containing
    // literal `..` (very rare, almost always a typo) collapse here too.
    .replace(/\.{2,}/g, ".");

  // Filename-level safety: empty / illegal chars / Windows reserved
  // names. Extracted into ../../lib/path-validation so Shift / Embed
  // resolvers on both CLI and GUI sides apply the same rules.
  assertSafeOutputFilename(resolved);

  // Build full output path.
  const outputPath = `${dir}/${resolved}`;

  // Path-level safety: traversal, dir-escape, MAX_PATH, self-overwrite.
  assertSafeOutputPath(outputPath, normalized);

  // HDR-specific: output must have .ass extension. Shift preserves the
  // input's extension (.srt → .srt) and Embed always emits .ass via
  // template — so this rule only belongs here.
  if (!resolved.toLowerCase().endsWith(".ass")) {
    throw new Error("Output filename must end with .ass");
  }

  // Restore native Windows separators on the final return value when the
  // input used them — keeps the output path shape consistent with the
  // input shape across downstream IPC writes and user-visible log lines.
  return usedBackslash ? outputPath.replace(/\//g, "\\") : outputPath;
}

/**
 * Text-cue subtitle → ASS conversion with color preprocessing.
 *
 * Converts SRT/WebVTT-ish <font color="#RRGGBB"> tags to ASS inline
 * color overrides before building a full ASS document. This allows the
 * HDR processor to handle all color tags uniformly.
 *
 * Pair with `src/lib/subtitle-parser.ts` for timing extraction, then
 * build a minimal ASS document for the HDR processor.
 */

import { decodeHTML } from "entities/decode";
import { ASCII_CONTROL_CHARS, BIDI_AND_ZERO_WIDTH_CHARS } from "../../lib/unicode-controls";
import { parseSubtitle, safeMs } from "../../lib/subtitle-parser";

// hoisted to module scope so the regex
// compiles once instead of per buildAssFromSrtBlocks invocation.
// Bidi + zero-width chars come from the shared rejection set
// (mirrors Rust-side validate_font_family + sanitizeForDialog).
// `Arial<U+202E>evil` would otherwise render visually reversed in
// editor previews. U+2028/2029 are included in the shared set so
// the prior explicit U+2028/U+2029 enumeration here is now covered.
//
// Sibling cross-ref: the extra `,{}\\:` literals on top of the BIDI /
// control set are SPECIFIC to ASS Style-line CSV shape — Style lines
// use `,` as field separator and `:` as the post-style-name terminator,
// so a font name carrying those would silently split the row. The
// sibling sanitizer in `src/features/font-embed/ass-uuencode.ts`
// (`safeName` inline regex inside `buildFontEntry`) adds `:/\\` instead
// — `:` delimits the `fontname: <name>` [Fonts] header line, and `/`
// / `\\` are defense-in-depth beyond the upstream buildFontFileName
// sanitizer. `:` and `\\` appear in BOTH sets — that is the structural
// overlap; `,{}` are unique to this side, `/` is unique to the
// ass-uuencode side. Both sanitizers must keep stripping the shared
// BIDI / control set; the extra
// boundary-specific chars are intentional and MUST NOT be unified into
// a single helper without re-checking the per-boundary character
// implications.
const FONT_NAME_SANITIZER = new RegExp(
  `[${ASCII_CONTROL_CHARS}${BIDI_AND_ZERO_WIDTH_CHARS},{}\\\\:]`,
  "gu"
);

// Matches a complete <font color="#RRGGBB"> or
// <font color=#RRGGBB> opener with up to 512 chars of other attributes
// before/after color. Anchoring lets the stateful scanner below identify
// every <font> frame while this regex alone decides whether the opener may
// inject a color override. The hex alternation requires a non-hex char
// immediately after the 6- or 3-digit run so `#abcdef` is never parsed as
// 3-digit `abc`.
const SRT_COLOR_OPEN_RE =
  /^<font\b[^>]{0,512}\bcolor="?#([0-9a-fA-F]{6}(?![0-9a-fA-F])|[0-9a-fA-F]{3}(?![0-9a-fA-F]))"?[^>]{0,512}>$/i;

// The longest opener accepted by SRT_COLOR_OPEN_RE is 1,045 characters:
// `<font` + two 512-character attribute windows + `color="#RRGGBB"` + `>`.
// Longer tags follow unknown-tag handling; they never inject an ASS override.
const MAX_SRT_FONT_TAG_LENGTH = 1_045;

// Production cue text is already capped at 64,000 characters, but keep the
// exported preprocessing helper safe when it is called directly. Overflow
// nesting is counted separately so an ignored closer cannot pop a tracked
// outer color frame early.
const MAX_TRACKED_SRT_FONT_DEPTH = 256;

type InlinePrimaryColor = string | null;

interface SrtFontFrame {
  previousColor: InlinePrimaryColor;
  setsColor: boolean;
}

// ── Text Cue Color Preprocessing ─────────────────────────

/**
 * Encode literal cue text before emitting any trusted ASS override tags.
 * @internal — production callers use the composed cue conversion below.
 */
export function escapeSrtUserText(text: string): string {
  // ASS has no \\ escape. FFmpeg also inserts WORD JOINER to keep literal
  // backslashes from forming \N, \h, or an escape of a following trusted tag.
  return text
    .replace(/\\(?!\u2060)/g, "\\\u2060")
    .replace(/\{/g, "\\{")
    .replace(/\}/g, "\\}");
}

/**
 * Convert HTML-style font color tags to ASS inline color overrides.
 * <font color="#RRGGBB">text</font>  →  {\1c&HBBGGRR&}text{\1c}
 *
 * Every bounded <font> opener gets a stack frame, including openers without
 * a supported color attribute. This preserves nesting: closing a non-color
 * frame cannot reset or pop an outer color. A nested color close restores the
 * previous inline color, while an outer color close uses bare `\1c` to restore
 * only the current ASS style's primary color. It deliberately does not emit
 * `\r`, because that would also erase bold, italic, underline, font, and other
 * active styling.
 *
 * This isolated color stage preserves other markup and assumes literal ASS
 * characters were already escaped. Production conversion uses the composed
 * cue serializer; this export permits focused color-stack tests.
 * @internal
 */
export function preprocessSrtColors(text: string): string {
  return processCueMarkup(
    text,
    (part) => part,
    (tag) => tag
  );
}

function processCueMarkup(
  text: string,
  renderText: (text: string) => string,
  renderTag: (tag: string) => string
): string {
  const output: string[] = [];
  const frames: SrtFontFrame[] = [];
  let currentColor: InlinePrimaryColor = null;
  let overflowDepth = 0;
  let cursor = 0;
  // Literal '<3' / '< $5' must not consume a later formatting tag. Recognize
  // tag names, declarations, empty tags, and WebVTT timestamps without rescanning.
  const tagStartPattern = /<(?:\/?[a-z]|[!?>]|\d{2,12}:)/gi;

  while (cursor < text.length) {
    tagStartPattern.lastIndex = cursor;
    const tagStartMatch = tagStartPattern.exec(text);
    if (!tagStartMatch) {
      output.push(renderText(text.slice(cursor)));
      break;
    }

    const tagStart = tagStartMatch.index;
    output.push(renderText(text.slice(cursor, tagStart)));
    const tagEnd = text.indexOf(">", tagStart + 1);
    if (tagEnd < 0) {
      // An unmatched opener makes the remaining tail literal; scanning it
      // again for every following '<' would make this path quadratic.
      output.push(renderText(text.slice(tagStart)));
      break;
    }
    const openTag = text.slice(tagStart, tagEnd + 1);
    cursor = tagEnd + 1;

    if (openTag.toLowerCase() === "</font>") {
      if (overflowDepth > 0) {
        overflowDepth -= 1;
        output.push(renderTag(openTag));
        continue;
      }

      const frame = frames.pop();
      if (!frame) {
        output.push(renderTag(openTag));
        continue;
      }

      if (!frame.setsColor) {
        output.push(renderTag(openTag));
        continue;
      }

      currentColor = frame.previousColor;
      output.push(currentColor === null ? "{\\1c}" : `{\\1c&H${currentColor}&}`);
      continue;
    }

    const openingPrefix = openTag.slice(0, 5).toLowerCase();
    const boundary = openTag[5];
    const isFontOpener =
      openingPrefix === "<font" && (boundary === undefined || !/[a-zA-Z0-9_]/.test(boundary));
    if (!isFontOpener) {
      output.push(renderTag(openTag));
      continue;
    }

    if (openTag.length > MAX_SRT_FONT_TAG_LENGTH || overflowDepth > 0) {
      overflowDepth += 1;
      output.push(renderTag(openTag));
      continue;
    }

    if (frames.length >= MAX_TRACKED_SRT_FONT_DEPTH) {
      overflowDepth = 1;
      output.push(renderTag(openTag));
      continue;
    }

    const colorMatch = SRT_COLOR_OPEN_RE.exec(openTag);
    frames.push({ previousColor: currentColor, setsColor: colorMatch !== null });
    if (!colorMatch) {
      output.push(renderTag(openTag));
      continue;
    }

    const raw = colorMatch[1]!;
    const hexRgb =
      raw.length === 3 ? raw[0]!.repeat(2) + raw[1]!.repeat(2) + raw[2]!.repeat(2) : raw;
    const r = hexRgb.slice(0, 2);
    const g = hexRgb.slice(2, 4);
    const b = hexRgb.slice(4, 6);
    currentColor = `${b}${g}${r}`;
    output.push(`{\\1c&H${currentColor}&}`);
  }

  return output.join("");
}

/**
 * Convert one parsed SRT/MicroDVD cue body into safe ASS text.
 */
export function processSrtUserText(text: string): string {
  return processTextCueUserText(text, false);
}

function processTextCueUserText(text: string, webVtt: boolean): string {
  return processCueMarkup(
    text,
    (plainText) => {
      // Decode original text tokens exactly once. Decoded '<' must stay text,
      // and decoded braces/backslashes must not become ASS instructions.
      const decoded = webVtt ? decodeHTML(plainText) : plainText;
      return escapeSrtUserText(decoded).replace(/\r\n|\r|\n|\u0085|\u2028|\u2029/g, "\\N");
    },
    (tag) => {
      if (/^<br\s*\/?>$/i.test(tag)) return "\\N";
      const style = /^<(\/?)([biu])>$/i.exec(tag);
      return style ? `{\\${style[2]!.toLowerCase()}${style[1] ? 0 : 1}}` : "";
    }
  );
}

// ── Style Configuration ──────────────────────────────────

export interface StyleConfig {
  fontName: string;
  fontSize: number;
  primaryColor: string; // ASS format: &H00FFFFFF
  outlineColor: string; // ASS format: &H00000000
  outlineWidth: number;
  shadowDepth: number;
}

export const DEFAULT_STYLE: StyleConfig = {
  fontName: "Arial",
  fontSize: 48,
  primaryColor: "&H00FFFFFF",
  outlineColor: "&H00000000",
  outlineWidth: 2.0,
  shadowDepth: 1.0,
};

// ── ASS Document Builder ─────────────────────────────────

/**
 * Build a minimal ASS document from entries with already prepared ASS text.
 * Cue markup and literal escaping belong to the composed cue serializer;
 * processing them again here would reinterpret decoded text or trusted tags.
 */
export function buildAssDocument(
  entries: { start: number; end: number; text: string }[],
  style: StyleConfig = DEFAULT_STYLE
): string {
  const lines: string[] = [];

  // [Script Info]
  lines.push("[Script Info]");
  lines.push("ScriptType: v4.00+");
  lines.push("PlayResX: 1920");
  lines.push("PlayResY: 1080");
  lines.push("WrapStyle: 0");
  lines.push("");

  // [V4+ Styles]
  lines.push("[V4+ Styles]");
  lines.push(
    "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding"
  );
  // Sanitize fontName: strip C0 + C1 control characters, commas (CSV
  // corruption), BiDi + zero-width + line/paragraph separators (shared
  // unicode-controls set), and ASS override-tag meta characters (`{`,
  // `}`, `\`, `:`). A user-typed name like `Arial{\fn...}`,
  // `Arial\u2028evil`, or `Arial<U+202E>evil` would otherwise smuggle
  // markup / line-break / visual-reversal semantics into the generated
  // Style line. Fall back to
  // "Arial" if sanitization empties the string — an empty Fontname field
  // produces a malformed Style CSV that ASS renderers treat unpredictably.
  // The regex lives at module scope as `FONT_NAME_SANITIZER` \u2014 see
  // definition above.
  // 128-codepoint cap matches `sanitizeFamily` (font-embedder).
  // Without it, a 10 KB font name typed into the HdrConvert style panel
  // would produce a 10 KB Style line.
  const safeFontName = style.fontName.replace(FONT_NAME_SANITIZER, "").slice(0, 128) || "Arial";
  lines.push(
    `Style: Default,${safeFontName},${style.fontSize},${style.primaryColor},&H000000FF,${style.outlineColor},&H00000000,0,0,0,0,100,100,0,0,1,${style.outlineWidth},${style.shadowDepth},2,10,10,10,1`
  );
  lines.push("");

  // [Events]
  lines.push("[Events]");
  lines.push("Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text");

  for (const entry of entries) {
    const startTime = msToAssTime(entry.start);
    const endTime = msToAssTime(entry.end);
    lines.push(`Dialogue: 0,${startTime},${endTime},Default,,0,0,0,,${entry.text}`);
  }

  return lines.join("\n");
}

export function buildAssDocumentFromCaptions(
  captions: { start: number; end: number; text: string; skipped?: boolean }[],
  style: StyleConfig = DEFAULT_STYLE
): { content: string; skippedCount: number } {
  if (captions.length === 0) {
    throw new Error("No subtitle cues detected");
  }

  const skippedCount = captions.filter((c) => c.skipped).length;
  const entries = captions
    .filter((c) => !c.skipped)
    .map((c) => ({
      start: c.start,
      end: c.end,
      text: c.text,
    }));

  if (entries.length === 0) {
    throw new Error(
      `No usable subtitle cues detected: all ${skippedCount} cue(s) exceeded the 64000-character limit`
    );
  }

  return {
    content: buildAssDocument(entries, style),
    skippedCount,
  };
}

/**
 * Convert a raw SRT, WebVTT, or MicroDVD document into ASS.
 *
 * Parse structure before touching user text: whole-document escaping would
 * corrupt MicroDVD `{start}{end}` fields. Only parsed cue bodies flow through
 * the composed escape/color pipeline, keeping hostile ASS overrides inert
 * without changing subtitle syntax. An explicit FPS is a manual MicroDVD
 * override; absence means Auto (file declaration, then 23.976 fallback).
 */
export function convertTextCueSubtitleToAss(
  rawContent: string,
  style: StyleConfig = DEFAULT_STYLE,
  fpsOverride?: number
): { content: string; skippedCount: number } {
  const { captions, format } = parseSubtitle(rawContent, fpsOverride);
  const processedCaptions = captions.map((caption) =>
    caption.skipped
      ? caption
      : { ...caption, text: processTextCueUserText(caption.text, format === "vtt") }
  );
  return buildAssDocumentFromCaptions(processedCaptions, style);
}

/**
 * Convert milliseconds to ASS timestamp format: H:MM:SS.cc (centiseconds)
 */
function msToAssTime(ms: number): string {
  ms = safeMs(ms);
  const totalCs = Math.round(ms / 10);
  const cs = totalCs % 100;
  const totalSec = Math.floor(totalCs / 100);
  const sec = totalSec % 60;
  const totalMin = Math.floor(totalSec / 60);
  const min = totalMin % 60;
  const hr = Math.floor(totalMin / 60);

  return `${hr}:${String(min).padStart(2, "0")}:${String(sec).padStart(2, "0")}.${String(cs).padStart(2, "0")}`;
}

// ── Format Support ────────────────────────────────────────

/** File extensions that need text-cue → ASS conversion */
export const CONVERTIBLE_EXTENSIONS = new Set([".srt", ".sub", ".vtt"]);

/** File extensions that are native ASS/SSA */
export const NATIVE_ASS_EXTENSIONS = new Set([".ass", ".ssa"]);

/** Check if a filename is a native ASS format */
export function isNativeAss(filename: string): boolean {
  const dotIdx = filename.lastIndexOf(".");
  if (dotIdx <= 0) return false;
  const ext = filename.slice(dotIdx).toLowerCase();
  return NATIVE_ASS_EXTENSIONS.has(ext);
}

/** Check if a filename can be converted to ASS */
export function isConvertible(filename: string): boolean {
  const dotIdx = filename.lastIndexOf(".");
  if (dotIdx <= 0) return false;
  const ext = filename.slice(dotIdx).toLowerCase();
  return CONVERTIBLE_EXTENSIONS.has(ext);
}

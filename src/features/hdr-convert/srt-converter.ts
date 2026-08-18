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
// Longer tags remain ordinary HTML-like text and are stripped later by the
// document builder; they never inject an ASS override.
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
 * Neutralize raw `\`, `{`, `}` in user-supplied SRT text BEFORE any of our
 * own ASS override tags are injected (via `preprocessSrtColors` or the
 * HTML-tag conversion in `buildAssDocument`). After this step runs, every
 * `{…}` seen downstream in the pipeline is a tag WE emitted — so nothing
 * can smuggle a libass override through user text.
 *
 * Callers MUST run this BEFORE `preprocessSrtColors` and must NOT re-escape
 * after; re-escaping would turn our trusted `{\1c&H…}` injections into
 * literal `\{\\1c&H…\}` text and silently break HDR color conversion.
 *
 * @internal — production callers must use `processSrtUserText` (composed
 * single entry point). Exported only so unit tests can exercise each
 * stage in isolation.
 */
export function escapeSrtUserText(text: string): string {
  return text.replace(/\\/g, "\\\\").replace(/\{/g, "\\{").replace(/\}/g, "\\}");
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
 * CONTRACT: the `text` argument MUST have been passed through
 * `escapeSrtUserText` first. That's the only way to guarantee the `{…}`
 * sequences this function injects for color conversion are distinguishable
 * from literal `{…}` in user-supplied text. Calling this on raw SRT content
 * re-introduces an injection path that lets a hostile subtitle smuggle ASS
 * overrides into the HDR pipeline.
 *
 * Production callers should prefer `processSrtUserText`, which composes
 * the two steps in the correct order. Direct exports remain for unit
 * tests that need to exercise each step in isolation.
 *
 * @internal — production callers must use `processSrtUserText`.
 */
export function preprocessSrtColors(text: string): string {
  const output: string[] = [];
  const frames: SrtFontFrame[] = [];
  let currentColor: InlinePrimaryColor = null;
  let overflowDepth = 0;
  let cursor = 0;

  while (cursor < text.length) {
    const tagStart = text.indexOf("<", cursor);
    if (tagStart < 0) {
      output.push(text.slice(cursor));
      break;
    }

    output.push(text.slice(cursor, tagStart));

    if (text.slice(tagStart, tagStart + 7).toLowerCase() === "</font>") {
      const closeTag = text.slice(tagStart, tagStart + 7);
      cursor = tagStart + 7;

      if (overflowDepth > 0) {
        overflowDepth -= 1;
        output.push(closeTag);
        continue;
      }

      const frame = frames.pop();
      if (!frame) {
        output.push(closeTag);
        continue;
      }

      if (!frame.setsColor) {
        output.push(closeTag);
        continue;
      }

      currentColor = frame.previousColor;
      output.push(currentColor === null ? "{\\1c}" : `{\\1c&H${currentColor}&}`);
      continue;
    }

    const openingPrefix = text.slice(tagStart, tagStart + 5).toLowerCase();
    const boundary = text[tagStart + 5];
    const isFontOpener =
      openingPrefix === "<font" && (boundary === undefined || !/[a-zA-Z0-9_]/.test(boundary));
    if (!isFontOpener) {
      output.push("<");
      cursor = tagStart + 1;
      continue;
    }

    const tagEnd = text.indexOf(">", tagStart + 5);
    if (tagEnd < 0) {
      output.push(text.slice(tagStart));
      break;
    }

    const openTag = text.slice(tagStart, tagEnd + 1);
    cursor = tagEnd + 1;

    if (openTag.length > MAX_SRT_FONT_TAG_LENGTH || overflowDepth > 0) {
      overflowDepth += 1;
      output.push(openTag);
      continue;
    }

    if (frames.length >= MAX_TRACKED_SRT_FONT_DEPTH) {
      overflowDepth = 1;
      output.push(openTag);
      continue;
    }

    const colorMatch = SRT_COLOR_OPEN_RE.exec(openTag);
    frames.push({ previousColor: currentColor, setsColor: colorMatch !== null });
    if (!colorMatch) {
      output.push(openTag);
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
 * Composed SRT-user-text pipeline: escape user text, then inject our
 * trusted color tags. This is the only entry point production callers
 * should use — it makes the contract between the two steps a single
 * function call rather than a documented call ordering, eliminating
 * the regression class where a future caller swaps the order or skips
 * the escape step.
 */
export function processSrtUserText(text: string): string {
  return preprocessSrtColors(escapeSrtUserText(text));
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
 * Build a minimal ASS document from parsed subtitle entries.
 * This creates a properly formatted ASS file with styles and events.
 *
 * CONTRACT: raw subtitle structure MUST be parsed first; each resulting cue
 * body must then flow through `escapeSrtUserText` → `preprocessSrtColors` on
 * the way in. This function does NOT re-escape `{`/`}`/`\` — doing so would silently
 * defeat our own injected color/bold/italic overrides and was the root of
 * a past regression. The integration tests in `srt-converter.test.ts`
 * guard against future callers dropping the escape step.
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
    // IMPORTANT: we do NOT escape `{` / `}` / `\` here. Callers are
    // expected to have already run `processSrtUserText` on this parsed cue
    // body. Whole-document escaping is forbidden because it corrupts
    // MicroDVD timing syntax. Re-escaping at this stage would turn injected
    // `{\1c&H…}` tags into literal text, silently defeating SRT→HDR color
    // conversion. See escapeSrtUserText's docstring for the required
    // pipeline ordering.
    const cleanText = entry.text
      // Normalize ALL line-break variants (LF, CRLF, bare CR, NEL,
      // LINE SEPARATOR U+2028, PARAGRAPH SEPARATOR U+2029) to the ASS
      // `\N` hard break. A bare `\r` would otherwise break the
      // one-line-per-Dialogue invariant; U+2028 smuggles a line break
      // past naive renderers.
      .replace(/\r\n|\r|\n|\u0085|\u2028|\u2029/g, "\\N")
      // Convert `<br>` / `<br/>` to ASS hard break BEFORE the
      // unknown-tag strip below. SRT is HTML-ish in many real-world
      // exports (legacy tools, fan-sub edits); without this step
      // intentional line breaks get silently absorbed by the
      // `<[^>]*>` strip pass and the cue collapses to a single line.
      .replace(/<br\s*\/?>/gi, "\\N")
      .replace(/<b>/gi, "{\\b1}")
      .replace(/<\/b>/gi, "{\\b0}")
      .replace(/<i>/gi, "{\\i1}")
      .replace(/<\/i>/gi, "{\\i0}")
      .replace(/<u>/gi, "{\\u1}")
      .replace(/<\/u>/gi, "{\\u0}")
      .replace(/<[^>]*>/g, ""); // strip remaining unknown HTML tags
    lines.push(`Dialogue: 0,${startTime},${endTime},Default,,0,0,0,,${cleanText}`);
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
  const { captions } = parseSubtitle(rawContent, fpsOverride);
  const processedCaptions = captions.map((caption) =>
    caption.skipped ? caption : { ...caption, text: processSrtUserText(caption.text) }
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

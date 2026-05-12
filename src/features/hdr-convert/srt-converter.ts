/**
 * SRT/SUB → ASS conversion with color preprocessing.
 *
 * Converts SRT <font color="#RRGGBB"> tags to ASS inline color overrides
 * before building a full ASS document. This allows the HDR processor
 * to handle all color tags uniformly.
 *
 * Uses subsrt for multi-format parsing and ass-compiler for ASS generation.
 */

import { BIDI_AND_ZERO_WIDTH_CHARS } from "../../lib/unicode-controls";

// ── SRT Color Preprocessing ──────────────────────────────

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
  // Regex defined inside function — no shared lastIndex state.
  // Matches: <font color="#RRGGBB"> or <font color=#RRGGBB>
  // with up to 512 chars of other attributes before/after color (ReDoS guard).
  // The hex alternation requires a non-hex char immediately after the 6- or
  // 3-digit run so `#abcdef` is never parsed as 3-digit `abc`.
  const SRT_COLOR_OPEN_RE =
    /<font\b[^>]{0,512}\bcolor="?#([0-9a-fA-F]{6}(?![0-9a-fA-F])|[0-9a-fA-F]{3}(?![0-9a-fA-F]))"?[^>]{0,512}>/gi;
  const SRT_COLOR_CLOSE_RE = /<\/font>/gi;

  // Convert opening tags with color
  let result = text.replace(SRT_COLOR_OPEN_RE, (_match, raw: string) => {
    const hexRgb = raw.length === 3 ? raw[0].repeat(2) + raw[1].repeat(2) + raw[2].repeat(2) : raw;
    const r = hexRgb.slice(0, 2);
    const g = hexRgb.slice(2, 4);
    const b = hexRgb.slice(4, 6);
    // Reverse to BGR for ASS format
    return `{\\1c&H${b}${g}${r}&}`;
  });

  // Convert ALL </font> to style resets — both color and non-color.
  // Non-color <font> tags are stripped later by HTML tag removal in buildAssDocument,
  // so their {\r} is harmless (resets to default which is the current state).
  // This avoids positional mismatch when non-color </font> precedes color </font>.
  result = result.replace(SRT_COLOR_CLOSE_RE, () => "{\\r}");

  return result;
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
  fps: number; // only used for SUB (MicroDVD) format
}

export const DEFAULT_STYLE: StyleConfig = {
  fontName: "Arial",
  fontSize: 48,
  primaryColor: "&H00FFFFFF",
  outlineColor: "&H00000000",
  outlineWidth: 2.0,
  shadowDepth: 1.0,
  fps: 23.976,
};

// ── ASS Document Builder ─────────────────────────────────

/**
 * Build a minimal ASS document from parsed subtitle entries.
 * This creates a properly formatted ASS file with styles and events.
 *
 * CONTRACT: each `entries[i].text` MUST have flowed through
 * `escapeSrtUserText` → `preprocessSrtColors` → `parseSubtitle` on the way
 * in. This function does NOT re-escape `{`/`}`/`\` — doing so would silently
 * defeat our own injected color/bold/italic overrides and was the root of
 * the Round 3 regression. The integration tests in `srt-converter.test.ts`
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
  // eslint-disable-next-line no-control-regex -- intentional: sanitize control chars from subtitle font names
  const fontNameSanitizer = new RegExp(
    // Bidi + zero-width chars come from the shared rejection set
    // (mirrors Rust-side validate_font_family + sanitizeForDialog).
    // `Arial<U+202E>evil` would otherwise render visually reversed in
    // editor previews. U+2028/2029 are included in the shared set so
    // the prior explicit \u2028\u2029 enumeration here is now covered.
    `[\\x00-\\x1f\\x7f-\\x9f${BIDI_AND_ZERO_WIDTH_CHARS},{}\\\\:]`,
    "gu",
  );
  const safeFontName = style.fontName.replace(fontNameSanitizer, "") || "Arial";
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
    // expected to have already run `escapeSrtUserText` on the ORIGINAL
    // SRT content before `preprocessSrtColors` injected our trusted color
    // override tags. Re-escaping at this stage would turn those injected
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

/**
 * Convert milliseconds to ASS timestamp format: H:MM:SS.cc (centiseconds)
 */
function msToAssTime(ms: number): string {
  // NaN / Infinity guard matches subtitle-parser.ts:formatAssTime — without
  // it, a malformed upstream caption (NaN start or end) would produce a
  // literal "NaN:NaN:NaN.NaN" timestamp and corrupt the whole file.
  if (!Number.isFinite(ms)) ms = 0;
  if (ms < 0) ms = 0;
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

/** File extensions that need SRT/SUB → ASS conversion */
export const CONVERTIBLE_EXTENSIONS = new Set([".srt", ".sub"]);

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

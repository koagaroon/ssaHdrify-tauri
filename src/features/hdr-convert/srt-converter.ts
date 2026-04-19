/**
 * SRT/SUB → ASS conversion with color preprocessing.
 *
 * Converts SRT <font color="#RRGGBB"> tags to ASS inline color overrides
 * before building a full ASS document. This allows the HDR processor
 * to handle all color tags uniformly.
 *
 * Uses subsrt for multi-format parsing and ass-compiler for ASS generation.
 */

// ── SRT Color Preprocessing ──────────────────────────────

/**
 * Convert HTML-style font color tags to ASS inline color overrides.
 * <font color="#RRGGBB">text</font>  →  {\1c&HBBGGRR&}text{\1c}
 */
export function preprocessSrtColors(text: string): string {
  // Regex defined inside function — no shared lastIndex state.
  // Matches: <font color="#RRGGBB"> or <font color=#RRGGBB>
  // with up to 512 chars of other attributes before/after color (ReDoS guard)
  const SRT_COLOR_OPEN_RE =
    /<font\b[^>]{0,512}\bcolor="?#([0-9a-fA-F]{6}|[0-9a-fA-F]{3})"?[^>]{0,512}>/gi;
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
  // Sanitize fontName: strip control characters, commas (CSV corruption), and
  // ASS override-tag meta characters (`{`, `}`, `\`) so a user-typed name like
  // `Arial{\fn...}` can't smuggle markup into the generated Style line. Fall
  // back to "Arial" if sanitization empties the string — an empty Fontname
  // field produces a malformed Style CSV that ASS renderers treat unpredictably.
  // eslint-disable-next-line no-control-regex -- intentional: sanitize control chars from subtitle font names
  const safeFontName = style.fontName.replace(/[\x00-\x1f\x7f,{}\\]/g, "") || "Arial";
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
    // Pipeline:
    //   1. Escape raw `{` / `}` from the SRT source FIRST. A user-supplied SRT
    //      with literal `{\an8\pos(0,0)}` in the text would otherwise survive
    //      into Dialogue as an active override and reposition / restyle the
    //      renderer — a silent-injection vector. `{` / `}` become `\{` / `\}`
    //      per libass convention.
    //   2. THEN inject our own trusted override tags (bold/italic/etc.) which
    //      use `{...}` by design — because we did step 1 first, the braces we
    //      emit here are never confused with user-escaped ones.
    const cleanText = entry.text
      .replace(/\\/g, "\\\\") // escape backslashes so they don't pair with following text
      .replace(/\{/g, "\\{")
      .replace(/\}/g, "\\}")
      .replace(/\r?\n/g, "\\N")
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

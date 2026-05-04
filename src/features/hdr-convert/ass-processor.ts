/**
 * ASS subtitle processor — parse ASS files and transform color tags to HDR.
 *
 * Handles two types of colors:
 * 1. Style colors (primary, secondary, outline, back) in [V4+ Styles] section
 * 2. Inline color tags in dialogue events: \c&HBBGGRR, \1c&HBBGGRR, etc.
 *
 * ASS color format: &H[AA]BBGGRR (BGR byte order, optional alpha prefix)
 */
import { sRgbToHdr, type Eotf, DEFAULT_BRIGHTNESS } from "./color-engine";

// ── Style color fields in ASS [V4+ Styles] section ────────
// Format line: "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, ..."
// Style line: "Style: Default,Arial,48,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,..."
// Color fields are at fixed indices: Primary=3, Secondary=4, Outline=5, Back=6
const STYLE_COLOR_INDICES_FALLBACK = [3, 4, 5, 6];

/** Names of the color fields in the ASS style format line */
const STYLE_COLOR_FIELDS = new Set([
  "primarycolour",
  "secondarycolour",
  "outlinecolour",
  "backcolour",
]);

/**
 * Parse a Format line in [V4+ Styles] section and return indices of color fields.
 * Returns null if parsing fails (caller should use fallback).
 */
function parseStyleFormatLine(formatLine: string): number[] | null {
  const colonIdx = formatLine.indexOf(":");
  if (colonIdx < 0) return null;
  const fields = formatLine
    .slice(colonIdx + 1)
    .split(",")
    .map((f) => f.trim().toLowerCase());
  const indices: number[] = [];
  for (let i = 0; i < fields.length; i++) {
    if (STYLE_COLOR_FIELDS.has(fields[i])) {
      indices.push(i);
    }
  }
  return indices.length > 0 ? indices : null;
}

/**
 * Parse an ASS color string (&H[AA]BBGGRR) into {r, g, b, alpha}.
 * Alpha: "00" = opaque, "FF" = fully transparent (ASS convention).
 */
export function parseAssColor(assColor: string): {
  r: number;
  g: number;
  b: number;
  alpha: string;
} {
  // Strip &H prefix, pad to 8 chars
  const stripped = assColor.replace(/^&H/i, "").padStart(8, "0");
  const alpha = stripped.slice(0, 2);
  const blue = parseInt(stripped.slice(2, 4), 16);
  const green = parseInt(stripped.slice(4, 6), 16);
  const red = parseInt(stripped.slice(6, 8), 16);

  if ([blue, green, red].some(Number.isNaN)) {
    return { r: 255, g: 255, b: 255, alpha: "00" }; // fallback: white opaque
  }
  return { r: red, g: green, b: blue, alpha };
}

/** Format a single byte as two uppercase hex digits. */
function hexByte(n: number): string {
  return n.toString(16).padStart(2, "0").toUpperCase();
}

/**
 * Format RGB back to ASS color string with preserved alpha.
 */
export function formatAssColor(r: number, g: number, b: number, alpha: string): string {
  return `&H${alpha}${hexByte(b)}${hexByte(g)}${hexByte(r)}`;
}

/**
 * Transform a single ASS color string from SDR to HDR.
 */
function transformColorString(assColor: string, targetBrightness: number, eotf: Eotf): string {
  const { r, g, b, alpha } = parseAssColor(assColor);
  const [hr, hg, hb] = sRgbToHdr(r, g, b, targetBrightness, eotf);
  return formatAssColor(hr, hg, hb, alpha);
}

// Matches: \c&HBBGGRR, \1c&HBBGGRR, \2c&HAABBGGRR, etc.
// Groups: (1) prefix like "\c&H" or "\1c&H", (2) 6 or 8 hex digits.
// Strict 6/8 length is per ASS spec — any other count (e.g. a 7-digit
// run from a malformed file) is intentionally left un-transformed
// rather than risk corrupting unknown content.
// Lookahead ensures the color ends at a valid ASS delimiter. `\r\n` are
// included so a color tag at end-of-line (within a multi-line ASS input
// before line-splitting) still matches instead of being left untransformed.
//
// Hoisted to module scope so a 50k-dialogue file doesn't re-compile this
// regex per line.
const COLOR_TAG_RE = /(\\[0-9]?c&H)([0-9a-fA-F]{6}|[0-9a-fA-F]{8})(?=[&}),\\\r\n]|$)/g;

/**
 * Transform inline color tags in a dialogue event text.
 * e.g., {\1c&H0000FF} → {\1c&H002D45}
 */
function transformEventText(text: string, targetBrightness: number, eotf: Eotf): string {
  return text.replace(COLOR_TAG_RE, (_, prefix: string, hexColor: string) => {
    const { r, g, b, alpha } = parseAssColor(`&H${hexColor}`);
    const [hr, hg, hb] = sRgbToHdr(r, g, b, targetBrightness, eotf);
    // Inline color tags use the same alpha prefix as the input
    const alphaPrefix = hexColor.length === 8 ? alpha : "";
    return `${prefix}${alphaPrefix}${hexByte(hb)}${hexByte(hg)}${hexByte(hr)}`;
  });
}

/**
 * Transform a Style line in the [V4+ Styles] section.
 * Style lines are CSV: "Style: Name,Fontname,Fontsize,PrimaryColour,..."
 */
function transformStyleLine(
  line: string,
  targetBrightness: number,
  eotf: Eotf,
  styleColorIndices: number[]
): string {
  // Match Style: prefix case-insensitively and tolerate leading whitespace
  // (ASS renderers accept both). The raw line is preserved after the colon
  // so any indentation / casing in the source file survives the transform.
  if (!/^\s*style:/i.test(line)) return line;

  const colonIdx = line.indexOf(":");
  const prefix = line.slice(0, colonIdx + 1);
  const afterColon = line.slice(colonIdx + 1);
  const fields = afterColon.split(",");

  // Only transform fields that actually look like ASS colors. A
  // crafted Format line with empty middle fields would shift the index
  // we computed from the Format header against this Style line; if the
  // shifted slot lands on something that isn't a color, transformColorString
  // would produce garbage and we'd silently corrupt the output. Validate
  // the &Hxxxxxx{6,8} shape before transforming — anything else falls
  // through untouched. Mirrors the strictness of COLOR_TAG_RE.
  const COLOR_FIELD_RE = /^&H[0-9A-Fa-f]{2,8}$/;
  for (const idx of styleColorIndices) {
    if (idx < fields.length && fields[idx]) {
      const trimmed = fields[idx].trim();
      if (COLOR_FIELD_RE.test(trimmed)) {
        fields[idx] = transformColorString(trimmed, targetBrightness, eotf);
      }
    }
  }

  return `${prefix}${fields.join(",")}`;
}

// ── Section Detection ─────────────────────────────────────

export type AssSection = "info" | "styles" | "fonts" | "events" | "other";

/**
 * Matches real ASS section headers while excluding UUEncode font data.
 * UUEncode output uses ASCII 33–96 which includes [ ] A-Z 0-9 + but
 * NEVER lowercase letters (97-122) or space (32). Every real ASS section
 * name contains at least one lowercase letter or space (e.g., [Script Info],
 * [V4+ Styles], [Events], [Fonts]). The lookahead ensures at least one such
 * character exists, making UUEncode collisions impossible.
 * Length capped at 50 as defense-in-depth (longest known: 24 inner chars).
 */
/** Callers must pass a trimmed, lowercased string for correct matching. */
export const SECTION_HEADER_RE = /^\[(?=[a-z0-9+ ]*[a-z ])[a-z0-9+ ]{1,50}\]$/;

export function detectSection(line: string): AssSection | null {
  const trimmed = line.trim().toLowerCase();
  if (trimmed === "[script info]") return "info";
  if (trimmed === "[v4+ styles]" || trimmed === "[v4 styles]") return "styles";
  if (trimmed === "[fonts]") return "fonts";
  if (trimmed === "[events]") return "events";
  // Catch-all for unknown section headers. Note: trimmed is already lowercased,
  // so the [a-z ] lookahead is always satisfied by any letter — this means
  // UUEncode lines could theoretically match here. This is acceptable because
  // the "other" branch in processAssContent passes lines through unchanged,
  // identical to the "fonts" branch. No correctness impact from a false positive.
  if (SECTION_HEADER_RE.test(trimmed)) return "other";
  return null; // not a section header
}

/**
 * Process a full ASS file: transform all colors from SDR to HDR.
 *
 * @param content - Full ASS file content as string
 * @param targetBrightness - Target luminance in nits
 * @param eotf - Transfer function: "PQ" or "HLG"
 * @param onProgress - Optional callback for progress (lineIndex, totalLines)
 * @returns Transformed ASS file content
 */
export function processAssContent(
  content: string,
  targetBrightness: number = DEFAULT_BRIGHTNESS,
  eotf: Eotf = "PQ",
  onProgress?: (current: number, total: number) => void
): string {
  // Pre-split byte-size guard — catches giant inputs BEFORE we allocate the
  // per-line array. Without this, a 500 MB blob splits into a huge array
  // (freeing memory only after the line-count throw fires). The Rust IPC
  // layer already caps reads at 50 MB, so 100 MB here is purely a
  // defense-in-depth budget for internally-generated content (e.g., SRT
  // expanded into ASS before re-processing).
  if (content.length > 100_000_000) {
    throw new Error(`File too large: ${(content.length / 1_000_000).toFixed(1)} MB (max 100 MB)`);
  }

  // Preserve the original line ending style
  const lineEnding = content.includes("\r\n") ? "\r\n" : "\n";
  const lines = content.split(/\r?\n/);

  if (lines.length > 500000) {
    throw new Error(`File too large: ${lines.length} lines (max 500,000)`);
  }

  let currentSection: AssSection = "info";
  const result: string[] = [];

  // Fresh per-call copy so a Format line parsed in one file doesn't leak its
  // color-field indices into the next file when callers reuse this module.
  let styleColorIndices = STYLE_COLOR_INDICES_FALLBACK.slice();

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];

    const newSection = detectSection(line);
    if (newSection !== null) {
      currentSection = newSection;
      result.push(line);
      continue;
    }

    if (currentSection === "styles") {
      // Format lines declare the color-field positions for later Style lines;
      // pass them through unchanged. Case-insensitive, tolerate leading
      // whitespace — same lenience the style branch applies below.
      const trimmedLeading = line.trimStart();
      if (/^format:/i.test(trimmedLeading)) {
        const parsed = parseStyleFormatLine(line);
        if (parsed) {
          styleColorIndices = parsed;
        }
        result.push(line);
      } else {
        result.push(transformStyleLine(line, targetBrightness, eotf, styleColorIndices));
      }
    } else if (currentSection === "events" && /^\s*dialogue:/i.test(line)) {
      // Comment: lines are non-rendering and must not be mutated.
      // Dialogue: match is case-insensitive and accepts leading whitespace so
      // renderers' lenience is mirrored here — otherwise "dialogue:" or
      // " Dialogue:" would slip through untransformed.
      result.push(transformEventText(line, targetBrightness, eotf));
    } else {
      result.push(line);
    }

    if (onProgress && i % 100 === 0) {
      onProgress(i, lines.length);
    }
  }

  // Signal completion — callers displaying progress need the final 100% tick
  onProgress?.(lines.length, lines.length);

  return result.join(lineEnding);
}

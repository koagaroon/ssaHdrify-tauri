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

// ── ASS Color Regex ───────────────────────────────────────
// Matches: \c&HBBGGRR, \1c&HBBGGRR, \2c&HAABBGGRR, etc.
// Groups: (1) prefix like "\c&H" or "\1c&H", (2) 6 or 8 hex digits
// Lookahead ensures the color ends at a valid ASS delimiter
const COLOR_TAG_RE =
  /(\\[0-9]?c&H)([0-9a-fA-F]{6}|[0-9a-fA-F]{8})(?=[&}),\\])/g;

// ── Style color fields in ASS [V4+ Styles] section ────────
// Format line: "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, ..."
// Style line: "Style: Default,Arial,48,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,..."
// Color fields are at fixed indices: Primary=3, Secondary=4, Outline=5, Back=6
const STYLE_COLOR_INDICES = [3, 4, 5, 6];

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

/**
 * Format RGB back to ASS color string with preserved alpha.
 */
export function formatAssColor(
  r: number,
  g: number,
  b: number,
  alpha: string
): string {
  const hex = (n: number) => n.toString(16).padStart(2, "0").toUpperCase();
  return `&H${alpha}${hex(b)}${hex(g)}${hex(r)}`;
}

/**
 * Transform a single ASS color string from SDR to HDR.
 */
function transformColorString(
  assColor: string,
  targetBrightness: number,
  eotf: Eotf
): string {
  const { r, g, b, alpha } = parseAssColor(assColor);
  const [hr, hg, hb] = sRgbToHdr(r, g, b, targetBrightness, eotf);
  return formatAssColor(hr, hg, hb, alpha);
}

/**
 * Transform inline color tags in a dialogue event text.
 * e.g., {\1c&H0000FF} → {\1c&H002D45}
 */
function transformEventText(
  text: string,
  targetBrightness: number,
  eotf: Eotf
): string {
  return text.replace(COLOR_TAG_RE, (_, prefix: string, hexColor: string) => {
    let alpha = "";
    let bgr = hexColor;

    // 8-digit: first 2 chars are alpha
    if (hexColor.length === 8) {
      alpha = hexColor.slice(0, 2);
      bgr = hexColor.slice(2);
    }

    // Pad to 6 digits for safety
    bgr = bgr.padStart(6, "0");

    const blue = parseInt(bgr.slice(0, 2), 16);
    const green = parseInt(bgr.slice(2, 4), 16);
    const red = parseInt(bgr.slice(4, 6), 16);

    const [hr, hg, hb] = sRgbToHdr(red, green, blue, targetBrightness, eotf);

    const hex = (n: number) => n.toString(16).padStart(2, "0");
    return `${prefix}${alpha}${hex(hb)}${hex(hg)}${hex(hr)}`;
  });
}

/**
 * Transform a Style line in the [V4+ Styles] section.
 * Style lines are CSV: "Style: Name,Fontname,Fontsize,PrimaryColour,..."
 */
function transformStyleLine(
  line: string,
  targetBrightness: number,
  eotf: Eotf
): string {
  if (!line.startsWith("Style:")) return line;

  // Split "Style: " prefix from the rest
  const colonIdx = line.indexOf(":");
  const prefix = line.slice(0, colonIdx + 1);
  const fields = line.slice(colonIdx + 1).split(",").map((f) => f.trim());

  // Transform color fields at known indices
  for (const idx of STYLE_COLOR_INDICES) {
    if (idx < fields.length && fields[idx]) {
      fields[idx] = transformColorString(fields[idx], targetBrightness, eotf);
    }
  }

  return `${prefix} ${fields.join(",")}`;
}

// ── Section Detection ─────────────────────────────────────

type AssSection = "info" | "styles" | "fonts" | "events" | "other";

function detectSection(line: string): AssSection | null {
  const trimmed = line.trim().toLowerCase();
  if (trimmed === "[script info]") return "info";
  if (trimmed === "[v4+ styles]" || trimmed === "[v4 styles]") return "styles";
  if (trimmed === "[fonts]") return "fonts";
  if (trimmed === "[events]") return "events";
  if (trimmed.startsWith("[")) return "other";
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
  const lines = content.split(/\r?\n/);
  let currentSection: AssSection = "info";
  const result: string[] = [];

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];

    // Check for section change
    const newSection = detectSection(line);
    if (newSection !== null) {
      currentSection = newSection;
      result.push(line);
      continue;
    }

    // Transform based on current section
    if (currentSection === "styles") {
      result.push(transformStyleLine(line, targetBrightness, eotf));
    } else if (currentSection === "events") {
      // Dialogue lines: "Dialogue: ..." — transform inline color tags
      if (line.startsWith("Dialogue:") || line.startsWith("Comment:")) {
        result.push(transformEventText(line, targetBrightness, eotf));
      } else {
        result.push(line);
      }
    } else {
      result.push(line);
    }

    if (onProgress && i % 100 === 0) {
      onProgress(i, lines.length);
    }
  }

  return result.join("\n");
}

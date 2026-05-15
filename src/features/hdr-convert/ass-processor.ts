/**
 * ASS subtitle processor — parse ASS files and transform color tags to HDR.
 *
 * Handles two types of colors:
 * 1. Style colors (primary, secondary, outline, back) in [V4+ Styles] section
 * 2. Inline color tags in dialogue events: \c&HBBGGRR, \1c&HBBGGRR, etc.
 *
 * ASS color format: &H[AA]BBGGRR (BGR byte order, optional alpha prefix)
 */
import { sRgbToHdr, type Eotf, DEFAULT_BRIGHTNESS, MIN_BRIGHTNESS } from "./color-engine";
import { MAX_PARSED_ENTRIES } from "../../lib/subtitle-parser";

// Round 10 N-R10-031: derive LINE_CAP from MAX_PARSED_ENTRIES plus
// a header-overhead budget so the cap accommodates SRT→ASS upcasts.
// Pre-R10 LINE_CAP was hardcoded to 500_000 (== MAX_PARSED_ENTRIES);
// an SRT input near the parser cap then expanded through
// buildAssDocument's ~11-line header would throw here on the
// re-pass even though parseSrt had accepted the file. 1024-line
// budget covers `[Script Info]` / `[V4+ Styles]` / Format / multiple
// Style rows / `[Events]` / Format / Comment lines / blank
// separators, well over the ~11-line minimum used by the standard
// builder and any plausible authoring tool's preamble.
const ASS_HEADER_LINE_BUDGET = 1024;
const LINE_CAP = MAX_PARSED_ENTRIES + ASS_HEADER_LINE_BUDGET;

// Round 11 W11.3 (A1-R11-02): per-line byte cap. File-level guards
// (100 MB total, LINE_CAP entries) bound the aggregate but not a
// single line's length. A crafted ASS with one 99 MB Dialogue line
// (rest of file tiny) passes both guards yet saturates the UI thread
// during regex matching + detectSection's trim/lowercase pass. 1 MB
// per line mirrors font-collector's MAX_DIALOGUE_TEXT_LEN and is
// generously above legit content (typical Dialogue: 50-2000 chars,
// complex karaoke ~10 KB, UUEncode rows ~60 chars).
const MAX_LINE_BYTES = 1_000_000;

// Round 11 W11.3 (A1-R11-03): cap on field count in a Style line's
// comma-split. ASS Style format defines ~24 fields; a hostile line
// with millions of commas would otherwise allocate a multi-MB array
// in transformStyleLine before any color-field index lookup runs.
// 1024 is far above any plausible authoring tool's output.
const MAX_STYLE_FIELDS = 1024;

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

// Round 7 Wave 7.6 (A4-R7-13): hoisted from inside transformStyleLine
// to match COLOR_TAG_RE's module-scope precedent. Previously the
// regex literal compiled per style-line; with thousands of styled
// dialogues a per-call recompile is wasted work, and `no-misleading-
// character-class` / consistency reads better when the two regexes
// sit side by side.
const COLOR_FIELD_RE = /^&H(?:[0-9A-Fa-f]{6}|[0-9A-Fa-f]{8})$/;

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
  // Round 11 W11.3 (A1-R11-03): bound the field-array size. See
  // MAX_STYLE_FIELDS docblock for the threat shape. Per-line byte cap
  // (MAX_LINE_BYTES) already bounds the comma count indirectly via
  // line length; this check is the explicit pair so a future loosening
  // of the per-line cap doesn't silently re-open the array-allocation
  // attack surface.
  if (fields.length > MAX_STYLE_FIELDS) {
    throw new Error(`Style line has too many fields: ${fields.length} (max ${MAX_STYLE_FIELDS})`);
  }

  // Only transform fields that actually look like ASS colors. A
  // crafted Format line with empty middle fields would shift the index
  // we computed from the Format header against this Style line; if the
  // shifted slot lands on something that isn't a color, transformColorString
  // would produce garbage and we'd silently corrupt the output. Validate
  // the `&Hxxxxxx` or `&Hxxxxxxxx` shape (6 or 8 hex digits — RGB or
  // ABGR with alpha) before transforming. Round 1 F2.N-R1-11: previous
  // `{2,8}` accepted 2-, 3-, 4-, 5-, and 7-digit values that the inline
  // `\cN&Hxxxxxx` tag regex (`COLOR_TAG_RE`, line 101) correctly
  // rejected — the asymmetry let a malformed Style field through that
  // wouldn't have been transformed if inlined as `\c&H...`.
  // (Round 7 Wave 7.6: regex now lives at module scope — see
  // `COLOR_FIELD_RE` definition.)
  for (const idx of styleColorIndices) {
    if (idx < fields.length && fields[idx]) {
      const raw = fields[idx];
      const trimmed = raw.trim();
      if (COLOR_FIELD_RE.test(trimmed)) {
        // Round 10 N-R10-030: preserve any leading/trailing whitespace
        // padding from the original field so byte-for-byte file
        // structure (excluding the color hex itself) survives the
        // transform. Pre-R10 the replacement used `trimmed` directly,
        // dropping the padding — ASS renderers tolerate it, but
        // diff-tooling against the input would surface the
        // whitespace shift as a spurious change.
        const leadingLen = raw.length - raw.trimStart().length;
        const trailingLen = raw.length - raw.trimEnd().length;
        const leading = raw.slice(0, leadingLen);
        const trailing = trailingLen > 0 ? raw.slice(raw.length - trailingLen) : "";
        const transformed = transformColorString(trimmed, targetBrightness, eotf);
        fields[idx] = `${leading}${transformed}${trailing}`;
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
  // Round 7 Wave 7.5 (A4-R7-2): same targetBrightness guard as
  // sRgbToHdr (defense-in-depth at the outer entry — if a future
  // caller bypasses sRgbToHdr's guard, e.g. via direct color-math
  // utility import, the file-level config still gets normalized).
  if (!Number.isFinite(targetBrightness) || targetBrightness < MIN_BRIGHTNESS) {
    targetBrightness = DEFAULT_BRIGHTNESS;
  }
  // Pre-split byte-size guard — catches giant inputs BEFORE we allocate the
  // per-line array. Without this, a 500 MB blob splits into a huge array
  // (freeing memory only after the line-count throw fires). The Rust IPC
  // layer already caps reads at 50 MB, so 100 MB here is purely a
  // defense-in-depth budget for internally-generated content (e.g., SRT
  // expanded into ASS before re-processing).
  if (content.length > 100_000_000) {
    throw new Error(`File too large: ${(content.length / 1_000_000).toFixed(1)} MB (max 100 MB)`);
  }

  // Pre-split line-count probe (A-R5-FEFEAT-03). 50 MB of pure '\n'
  // passes the byte-size guard above, but `.split(/\r?\n/)` then
  // allocates ~50M empty strings (~2 GB V8 heap) BEFORE the post-split
  // throw at line 221 can fire. A small content+pure-newline blob
  // crafted via a hostile subtitle file is reachable (P1b: subtitle
  // content from public release channels). Probe the count manually
  // and throw before the split allocates. Gated on content.length to
  // keep the small-file fast path zero-overhead. The 1 MB gate is well
  // above any realistic small subtitle (5-200 KB) and well below the
  // attack threshold (tens of MB).
  if (content.length > 1_000_000) {
    let nl = 1;
    for (let i = 0; i < content.length; i++) {
      if (content.charCodeAt(i) === 10 /* '\n' */) {
        nl++;
        if (nl > LINE_CAP) {
          throw new Error(`File too large: >${LINE_CAP} lines`);
        }
      }
    }
  }

  // Preserve the original line ending style
  const lineEnding = content.includes("\r\n") ? "\r\n" : "\n";
  const lines = content.split(/\r?\n/);

  if (lines.length > LINE_CAP) {
    throw new Error(`File too large: ${lines.length} lines (max ${LINE_CAP})`);
  }

  let currentSection: AssSection = "info";
  const result: string[] = [];

  // Fresh per-call copy so a Format line parsed in one file doesn't leak its
  // color-field indices into the next file when callers reuse this module.
  let styleColorIndices = STYLE_COLOR_INDICES_FALLBACK.slice();

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];

    // Round 11 W11.3 (A1-R11-02): per-line byte cap fires BEFORE
    // detectSection / transformStyleLine / transformEventText so a
    // single pathological line can't burn 99 MB of trim/lowercase +
    // regex work on the UI thread. See MAX_LINE_BYTES docblock.
    if (line.length > MAX_LINE_BYTES) {
      throw new Error(`Line ${i + 1} too long: ${line.length} chars (max ${MAX_LINE_BYTES})`);
    }

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

    // Skip the i === 0 fire (N-R5-FEFEAT-01): `0 % 100 === 0` would
    // emit a 0% update on every call, which for small files makes the
    // UI jump 0% → 100% with no intermediate ticks. Starting at the
    // first real boundary keeps the progress bar smooth.
    if (onProgress && i > 0 && i % 100 === 0) {
      onProgress(i, lines.length);
    }
  }

  // Signal completion — callers displaying progress need the final 100% tick
  onProgress?.(lines.length, lines.length);

  return result.join(lineEnding);
}

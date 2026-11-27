/**
 * Font collector — analyze ASS files to determine which fonts and glyphs are used.
 *
 * Algorithm (based on Aegisub's FontCollector):
 * 1. Parse ASS with ass-compiler → styles[] and dialogues[]
 * 2. Build a style→font map from [V4+ Styles]
 * 3. Walk each dialogue line's override tags (\fn, \b, \i)
 * 4. Track which characters are used with each font variant
 *
 * Result: Map of FontKey → Set of Unicode codepoints
 */

import type { ParsedASS } from "ass-compiler";
import { iterateSourceLines } from "../../lib/source-lines";
import { MAX_PARSED_ENTRIES } from "../../lib/subtitle-parser";
import { ASCII_CONTROL_CHARS, BIDI_AND_ZERO_WIDTH_CHARS } from "../../lib/unicode-controls";

// libass parses a non-parenthesized override argument through the next
// backslash, opening parenthesis, or end of the override block. Supported
// style names are looked up exactly after trailing ASCII spaces/tabs are removed.
// Names over the supported 128-codepoint style-name limit are consumed as an
// unknown reset so they cannot alias a valid prefix. The surrounding dialogue
// cap bounds this linear full-token scan.
const R_TAG_RE = /\\r([^\\(]*)/gu;
// Display-spoofing chars rejected from font family names. NOT added to
// unicode-controls.ts's BIDI_AND_ZERO_WIDTH_CHARS — that set is mirrored
// codepoint-for-codepoint to Rust `util.rs` and extended only on a CVE (see
// its header). These aren't crashes or Trojan-Source vectors:
//   - U+00A0 NBSP, U+2000-U+200A (variable-width spaces), U+202F narrow
//     NBSP, U+205F medium math space all masquerade as ASCII space, so a
//     family label / [Fonts] header can read as a different name than it is.
//   - Astral code points (U+10000-U+10FFFF) never legitimately appear in a
//     font family name.
// U+3000 (ideographic space) is intentionally NOT rejected — it is visibly
// full-width and legitimately used in CJK typography. These codepoints are
// stripped by `sanitizeFamily`, but they must NOT terminate FN_TAG_RE; otherwise
// `\fnArial<NBSP>Black` captures the prefix `Arial` and sanitization never sees
// the full spoofed family name.
const FAMILY_SPOOFING_CHARS = "\\u00A0\\u2000-\\u200A\\u202F\\u205F\\u{10000}-\\u{10FFFF}";

// Capture full raw arguments so bare, signed, and otherwise invalid values
// still participate in the state machine.
const FN_TAG_RE = /\\fn([^\\(]*)/gu;
// Exclude longer tags that libass dispatches before the one-letter forms.
const B_TAG_RE = /\\b(?!lur|ord|e)([^\\(]*)/g;
const I_TAG_RE = /\\i(?!clip)([^\\(]*)/g;
const P_TAG_RE = /\\p(?!bo|os)([^\\(]*)/g;

export type AssParseFunction = (text: string) => ParsedASS;

const MAX_ASS_CONTENT_LENGTH = 100_000_000;
const ASS_HEADER_LINE_BUDGET = 1024;
const MAX_COLLECT_LINES = MAX_PARSED_ENTRIES + ASS_HEADER_LINE_BUDGET;
const MAX_COLLECT_FIELDS = 1024;
const RAW_SECTION_HEADER_RE = /^\[(?=[A-Za-z0-9+ ]*[a-z ])[A-Za-z0-9+ ]{1,50}\]$/;
const KNOWN_UPPER_SECTIONS = new Set([
  "[script info]",
  "[v4+ styles]",
  "[v4 styles]",
  "[fonts]",
  "[events]",
  "[graphics]",
]);

/**
 * Defense-in-depth caps against crafted ASS input. See `collectFonts` for how
 * they interact — per-variant + total are both needed; either alone can be
 * blown past by the other dimension.
 *
 * MAX_CODEPOINTS_PER_VARIANT (65536) is a defensive cap, not the Basic
 * Multilingual Plane boundary (which caps at U+FFFF). Real fonts carry well
 * under this; the cap only fires against crafted ASS enumerating tens of
 * thousands of distinct characters for one font.
 */
const MAX_FONT_VARIANTS = 500;
const MAX_CODEPOINTS_PER_VARIANT = 65536;
const MAX_TOTAL_CODEPOINTS = 1_000_000;

export interface FontKey {
  family: string;
  bold: boolean;
  italic: boolean;
}

export interface FontUsage {
  key: FontKey;
  codepoints: Set<number>;
}

export interface FontStyleLabels {
  bold: string;
  italic: string;
}

const DEFAULT_FONT_STYLE_LABELS: Readonly<FontStyleLabels> = {
  bold: "Bold",
  italic: "Italic",
};

/**
 * Serialize a FontKey to a stable string for Map keys.
 */
function fontKeyToString(key: FontKey): string {
  return `${key.family}|${key.bold ? "B" : ""}${key.italic ? "I" : ""}`;
}

/** Format a FontKey as a human-readable label (e.g., "Arial Bold Italic"). */
export function fontKeyLabel(
  key: FontKey,
  styleLabels: Readonly<FontStyleLabels> = DEFAULT_FONT_STYLE_LABELS
): string {
  let label = key.family;
  if (key.bold) label += ` ${styleLabels.bold}`;
  if (key.italic) label += ` ${styleLabels.italic}`;
  return label;
}

/**
 * Strip the ASS `@` vertical-writing prefix from a family name.
 *
 * `@FamilyName` in a Style or `\fn` override tag tells the renderer to
 * rotate glyphs 90° for vertical typesetting — the underlying font file is
 * identical to the non-prefixed form. For font *identification* (matching,
 * subsetting, embedding) we must treat both as the same font, so this strip
 * is applied consistently wherever the collector captures a family name.
 */
function normalizeFamily(raw: string): string {
  const trimmed = raw.trim();
  return trimmed.startsWith("@") ? trimmed.slice(1) : trimmed;
}

/** Strip control characters and cap length — applied to every family name
 *  captured from a subtitle file before it flows into matching or output.
 *
 *  Note on naming: despite the `sanitize` prefix, this helper
 *  performs NORMALIZATION (strip + truncate) — it never
 *  rejects, only transforms. Compare to Rust-side `validate_font_family`
 *  which throws on the same codepoint set. The two roles are
 *  intentionally different:
 *
 *  - TS `sanitizeFamily` runs at the TS engine layer (parse a subtitle
 *    → collect font usages → present in detection grid). The family
 *    name is the user's content; we want to display it (possibly
 *    truncated and scrubbed) rather than refuse the whole subtitle.
 *
 *  - Rust `validate_font_family` runs at IPC entry and SQL-bind
 *    boundaries. Hostile inputs reaching the trust set / persistence
 *    layer SHOULD be rejected, not silently normalized into a
 *    different family name (which could shadow a legitimate font row).
 *
 *  A future "fix the asymmetry" refactor that tightens TS to reject
 *  would break legitimate inputs (a subtitle with a BiDi-bearing font
 *  name would refuse to render); a refactor that loosens Rust to
 *  normalize would smuggle hostile content past the trust gate. The
 *  asymmetry is load-bearing — keep both.
 *
 *  Range covers C0 (0x00-0x1F), DEL (0x7F), C1 (0x80-0x9F), the full
 *  BiDi / zero-width control set from `unicode-controls.ts` (which already
 *  includes the Unicode line/paragraph separators U+2028 / U+2029 — they
 *  are NOT a separate additive group), AND the display-spoofing whitespace
 *  + astral set (`FAMILY_SPOOFING_CHARS`). `\fn` captures those
 *  characters first and calls this sanitizer afterward so spoofing
 *  characters cannot become regex prefix boundaries.
 *  Previously a family name carrying U+202E could flow through
 *  `sanitizeFamily` into detection-grid labels, log lines, and chain
 *  progress text where the visual-reversal attack re-surfaced after
 *  the inline `safeName` regex inside `buildFontEntry`
 *  (ass-uuencode.ts) had already scrubbed it on the [Fonts] header
 *  path. Full parity with that inline regex on the shared codepoints
 *  — see `sanitization.test.ts` for the pin.
 *
 *  Exported for the cross-helper symmetry pin test: the parity claim
 *  between this helper and the inline regex inside
 *  `ass-uuencode::buildFontEntry` (NOT a named export — `safeName` is
 *  a local `const` inside that function; grep "BIDI_AND_ZERO_WIDTH"
 *  to find both consumers) is enforced by a test that exercises both
 *  sides on the same input range. */
export function sanitizeFamily(raw: string): string {
  // Control characters reach the regex via the dynamically-built
  // `new RegExp(...)` form rather than a regex literal — eslint's
  // `no-control-regex` only inspects literals, so no inline disable
  // directive is needed. Behavior is identical to a literal regex
  // (codepoint classes are evaluated at the same runtime stage).
  return raw
    .replace(
      new RegExp(
        `[${ASCII_CONTROL_CHARS}${BIDI_AND_ZERO_WIDTH_CHARS}${FAMILY_SPOOFING_CHARS}]`,
        "gu"
      ),
      ""
    )
    .slice(0, 128);
}

/**
 * Retain async readiness entrypoint for API compatibility.
 * `collectFonts` now uses the native bounded ASS reader directly, so no
 * dynamic `ass-compiler` chunk needs to be fetched at runtime.
 */
export async function ensureLoaded(): Promise<void> {
  return;
}

type CollectorSection = "styles" | "events" | "other";

function detectCollectorSection(rawLine: string, isFirstLine: boolean): CollectorSection | null {
  const line = isFirstLine && rawLine.startsWith("\uFEFF") ? rawLine.slice(1) : rawLine;
  const trimmed = line.trim();
  if (!trimmed.startsWith("[") || !trimmed.endsWith("]")) return null;
  const lower = trimmed.toLowerCase();
  if (lower === "[v4+ styles]" || lower === "[v4 styles]") return "styles";
  if (lower === "[events]") return "events";
  if (KNOWN_UPPER_SECTIONS.has(lower) || RAW_SECTION_HEADER_RE.test(trimmed)) return "other";
  return null;
}

interface StyleColumnMap {
  nameIdx: number;
  fontnameIdx: number;
  boldIdx: number;
  italicIdx: number;
}

const DEFAULT_STYLE_COLUMNS: StyleColumnMap = {
  nameIdx: 0,
  fontnameIdx: 1,
  boldIdx: 7,
  italicIdx: 8,
};

function parseStyleColumns(line: string): StyleColumnMap {
  const fields = line
    .slice(line.indexOf(":") + 1)
    .split(",", MAX_COLLECT_FIELDS + 1)
    .map((f) => f.trim().toLowerCase());
  if (fields.length > MAX_COLLECT_FIELDS) {
    throw new Error(`ASS/SSA style Format has too many fields (max ${MAX_COLLECT_FIELDS})`);
  }
  return {
    nameIdx: fields.indexOf("name"),
    fontnameIdx: fields.indexOf("fontname"),
    boldIdx: fields.indexOf("bold"),
    italicIdx: fields.indexOf("italic"),
  };
}

interface EventColumnMap {
  styleIdx: number;
  textIdx: number;
}

const DEFAULT_EVENT_COLUMNS: EventColumnMap = {
  styleIdx: 3,
  textIdx: 9,
};

function parseEventColumns(line: string): EventColumnMap {
  const fields = line
    .slice(line.indexOf(":") + 1)
    .split(",", MAX_COLLECT_FIELDS + 1)
    .map((f) => f.trim().toLowerCase());
  if (fields.length > MAX_COLLECT_FIELDS) {
    throw new Error(`ASS/SSA event Format has too many fields (max ${MAX_COLLECT_FIELDS})`);
  }
  if (fields.filter((f) => f === "text").length !== 1 || fields.at(-1) !== "text") {
    throw new Error("ASS/SSA event Format must contain one final Text field");
  }
  return {
    styleIdx: fields.indexOf("style"),
    textIdx: fields.length - 1,
  };
}

function extractDialogueStyleAndText(
  line: string,
  columns: EventColumnMap
): { rawStyle: string; rawText: string } | null {
  let cursor = line.indexOf(":") + 1;
  let rawStyle = "";
  for (let fieldIdx = 0; fieldIdx < columns.textIdx; fieldIdx += 1) {
    const comma = line.indexOf(",", cursor);
    if (comma < 0) return null;
    if (fieldIdx === columns.styleIdx) {
      rawStyle = line.slice(cursor, comma).trim();
    }
    cursor = comma + 1;
  }
  // Slice Text verbatim without `.trim()` so leading U+3000 fullwidth spaces
  // and trailing glyphs remain visible to glyph subsetting.
  return { rawStyle, rawText: line.slice(cursor) };
}

function createUsageAccumulator() {
  const usageMap = new Map<string, FontUsage>();
  let totalCodepoints = 0;

  function recordChars(key: FontKey, text: string) {
    const keyStr = fontKeyToString(key);
    let usage = usageMap.get(keyStr);
    if (!usage) {
      usage = { key: { ...key }, codepoints: new Set() };
      usageMap.set(keyStr, usage);
      if (usageMap.size > MAX_FONT_VARIANTS) {
        throw new Error(`Too many font variants: ${usageMap.size} (max ${MAX_FONT_VARIANTS})`);
      }
    }
    for (const char of text) {
      const cp = char.codePointAt(0);
      if (cp === undefined || cp <= 32 || cp === 0x7f || cp > 0x10ffff) {
        continue;
      }
      if (usage.codepoints.has(cp)) {
        continue;
      }
      if (usage.codepoints.size >= MAX_CODEPOINTS_PER_VARIANT) {
        throw new Error(
          `Too many codepoints for one font variant: ${usage.codepoints.size}+ (max ${MAX_CODEPOINTS_PER_VARIANT})`
        );
      }
      usage.codepoints.add(cp);
      totalCodepoints++;
      if (totalCodepoints > MAX_TOTAL_CODEPOINTS) {
        throw new Error(
          `Too many codepoints across fonts: ${totalCodepoints} (max ${MAX_TOTAL_CODEPOINTS})`
        );
      }
    }
  }

  return { usageMap, recordChars };
}

function resolveDialogueBaseStyle(
  rawStyleName: string,
  styleMap: ReadonlyMap<string, FontKey>
): FontKey {
  const effectiveRaw = rawStyleName || "Default";
  const unstarredStyleName = effectiveRaw.replace(/^\*+/u, "");
  const styleName = unstarredStyleName.toLowerCase() === "default" ? "Default" : unstarredStyleName;
  return (
    styleMap.get(styleName) ??
    styleMap.get("Default") ?? {
      family: "Arial",
      bold: false,
      italic: false,
    }
  );
}

/**
 * Collect font usage from an ASS/SSA file using a bounded streaming line reader.
 *
 * Preserves bare-`\r` Mac line endings, `[V4 Styles]` SSA headers, `*`-prefixed
 * style definitions, and leading `U+3000` ideographic spaces in dialogue text
 * without materializing a full-document AST.
 */
export function collectFonts(assContent: string): FontUsage[] {
  if (assContent.length > MAX_ASS_CONTENT_LENGTH) {
    throw new Error(
      `File too large: ${(assContent.length / 1_000_000).toFixed(1)} MB (max 100 MB)`
    );
  }

  const styleMap = new Map<string, FontKey>();
  const { usageMap, recordChars } = createUsageAccumulator();

  let section: CollectorSection = "other";
  let styleColumns: StyleColumnMap = DEFAULT_STYLE_COLUMNS;
  let eventColumns: EventColumnMap = DEFAULT_EVENT_COLUMNS;
  let lineCount = 1;

  for (const sourceLine of iterateSourceLines(assContent)) {
    const rawLine = sourceLine.body;
    if (rawLine.length > MAX_DIALOGUE_TEXT_LEN + 4096) {
      throw new Error(`Dialogue text too long: ${rawLine.length}+ (max ${MAX_DIALOGUE_TEXT_LEN})`);
    }
    if (sourceLine.ending && ++lineCount > MAX_COLLECT_LINES) {
      throw new Error(`File too large: >${MAX_COLLECT_LINES} lines`);
    }

    const detected = detectCollectorSection(rawLine, sourceLine.start === 0);
    if (detected !== null) {
      section = detected;
      continue;
    }

    if (section === "styles") {
      if (/^\s*format:/i.test(rawLine)) {
        styleColumns = parseStyleColumns(rawLine);
      } else if (/^\s*style:/i.test(rawLine)) {
        const fields = rawLine.slice(rawLine.indexOf(":") + 1).split(",", MAX_COLLECT_FIELDS + 1);
        if (fields.length > MAX_COLLECT_FIELDS) {
          throw new Error(
            `ASS/SSA Style line has too many fields: ${fields.length} (max ${MAX_COLLECT_FIELDS})`
          );
        }
        const rawName =
          styleColumns.nameIdx >= 0 ? (fields[styleColumns.nameIdx]?.trim() ?? "") : "";
        const styleName = rawName.replace(/^\*+/u, "");
        if (!styleName) continue;
        const rawFont =
          styleColumns.fontnameIdx >= 0 ? (fields[styleColumns.fontnameIdx]?.trim() ?? "") : "";
        const family = sanitizeFamily(normalizeFamily(rawFont || "Arial")) || "Arial";
        const rawBold =
          styleColumns.boldIdx >= 0 ? (fields[styleColumns.boldIdx]?.trim() ?? "0") : "0";
        const rawItalic =
          styleColumns.italicIdx >= 0 ? (fields[styleColumns.italicIdx]?.trim() ?? "0") : "0";
        styleMap.set(styleName, {
          family,
          bold: (Number.parseInt(rawBold, 10) || 0) !== 0,
          italic: (Number.parseInt(rawItalic, 10) || 0) !== 0,
        });
      }
    } else if (section === "events") {
      if (/^\s*format:/i.test(rawLine)) {
        eventColumns = parseEventColumns(rawLine);
      } else if (/^\s*dialogue:/i.test(rawLine)) {
        const extracted = extractDialogueStyleAndText(rawLine, eventColumns);
        if (!extracted) continue;
        const baseStyle = resolveDialogueBaseStyle(extracted.rawStyle, styleMap);
        processDialogueText(extracted.rawText, baseStyle, styleMap, recordChars);
      }
    }
  }

  return Array.from(usageMap.values());
}

export function collectFontsWithParser(assContent: string, parser: AssParseFunction): FontUsage[] {
  const parsed = parser(assContent);
  if (!parsed) {
    throw new Error("Failed to parse ASS file");
  }

  const styleMap = new Map<string, FontKey>();
  if (parsed.styles?.style) {
    for (const style of parsed.styles.style) {
      const family = sanitizeFamily(normalizeFamily(style.Fontname || "Arial"));
      const styleName = (style.Name ?? "").replace(/^\*+/u, "");
      if (!styleName) continue;
      styleMap.set(styleName, {
        family: family || "Arial",
        bold: (Number.parseInt(style.Bold || "0", 10) || 0) !== 0,
        italic: (Number.parseInt(style.Italic || "0", 10) || 0) !== 0,
      });
    }
  }

  const { usageMap, recordChars } = createUsageAccumulator();
  if (parsed.events?.dialogue) {
    for (const dialogue of parsed.events.dialogue) {
      const baseStyle = resolveDialogueBaseStyle(dialogue.Style || "Default", styleMap);
      const rawText: string = dialogue.Text?.raw ?? "";
      processDialogueText(rawText, baseStyle, styleMap, recordChars);
    }
  }

  return Array.from(usageMap.values());
}

/**
 * Parse a dialogue line's text, tracking font changes from override blocks.
 *
 * ASS override blocks: { ... } contain tags like \fnArial, \b1, \i1
 * Everything outside braces is rendered text.
 */
// Per-text length cap. ass-compiler returns the parsed dialogues; an
// upstream parser bug surfacing a giant text in a small input would
// drive O(n²) behavior on brace-light strings (the `text.indexOf("{",
// i)` scans + the per-char step compound). Rust caps total file size at
// 50 MB, so the cumulative budget is bounded — but a single dialogue
// near that ceiling is still pathological. 1 MB per dialogue is
// generous (typical line is 50-500 chars; even concatenated styled
// karaoke songs rarely cross a few KB).
const MAX_DIALOGUE_TEXT_LEN = 1_000_000;

function decodePlainText(text: string): string {
  return text.replace(/\\([Nnh{}])/g, (_, escaped: string) =>
    escaped === "{" || escaped === "}" ? escaped : ""
  );
}

function processDialogueText(
  text: string,
  eventStyle: FontKey,
  styleMap: Map<string, FontKey>,
  recordChars: (key: FontKey, text: string) => void
) {
  if (text.length > MAX_DIALOGUE_TEXT_LEN) {
    // throw rather than silently truncate
    // — parity with MAX_FONT_VARIANTS / MAX_CODEPOINTS_PER_VARIANT /
    // MAX_TOTAL_CODEPOINTS. An earlier slice() form lost glyphs from
    // the font analysis, producing a
    // subsetted font that silently missed characters present in the
    // source dialogue. The cap is 1 MB; legitimate ASS dialogues are
    // 50-500 chars, so hitting it means hostile or corrupt input
    // worth surfacing as a hard error the user can act on.
    throw new Error(`Dialogue text too long: ${text.length}+ (max ${MAX_DIALOGUE_TEXT_LEN})`);
  }
  let current = { ...eventStyle };
  // `currentStyle` is the active style baseline. Inline `\fn`, `\b`, and
  // `\i` overrides change `current` only; a successful `\rNamed` changes
  // both. Bare or numerically invalid `\b`/`\i` tags reset against this baseline.
  let currentStyle = { ...eventStyle };
  let isDrawing = false;
  let i = 0;

  while (i < text.length) {
    if (text[i] === "{" && (i === 0 || text[i - 1] !== "\\")) {
      // Override block — parse tags until closing }
      const closeIdx = text.indexOf("}", i);
      if (closeIdx === -1) {
        // Malformed override block — treat unmatched '{' as literal text
        // (matches behavior of most ASS renderers like libass/Aegisub).
        // Record all remaining text as rendered glyphs, then stop. We
        // used to `i++; continue;` which is O(n²) on pathological input
        // like `{{{{{…{` — each `{` would indexOf-scan to end of string.
        // Treating the tail as plain text is equivalent under libass's
        // "unmatched-brace means literal" semantics and finishes in O(n).
        if (!isDrawing) {
          const tail = text.slice(i);
          // Strip ASS drawing commands (\N, \n, \h) just like the
          // plain-text branch below . Without this,
          // input like `Hello{World\Nfoo` would record literal `\` + `N`
          // codepoints against the per-variant + total caps even
          // though libass treats them as line/space tags, not text.
          // one alternation pass instead of three
          // sequential replaces. Each `.replace(...)` allocates a fresh
          // intermediate string; for a 1 MB malformed-brace tail packed
          // with `\N` / `\n` / `\h`, three passes allocated ~3 MB of
          // intermediate strings. Single alternation is semantic-identical.
          const cleanTail = decodePlainText(tail);
          if (cleanTail.length > 0) recordChars(current, cleanTail);
        }
        return;
      }

      const block = text.slice(i + 1, closeIdx);
      const overrideResult = applyOverrideTags(
        block,
        current,
        currentStyle,
        isDrawing,
        eventStyle,
        styleMap
      );
      current = overrideResult.font;
      currentStyle = overrideResult.style;
      isDrawing = overrideResult.isDrawing;
      i = closeIdx + 1;
    } else {
      // Plain text — find the next override block or end
      let nextBrace = text.indexOf("{", i);
      // The converter emits libass's literal-brace escapes. Those braces
      // cannot begin an override, even when another backslash precedes them.
      while (nextBrace > 0 && text[nextBrace - 1] === "\\") {
        nextBrace = text.indexOf("{", nextBrace + 1);
      }
      const plainEnd = nextBrace >= 0 ? nextBrace : text.length;
      const plain = text.slice(i, plainEnd);

      // Skip ASS drawing commands (\N, \n, \h) and line breaks.
      // combined alternation (one allocator pass, was
      // three sequential .replace calls) — see the malformed-brace
      // tail path above for the rationale.
      const cleanText = decodePlainText(plain);

      if (cleanText.length > 0 && !isDrawing) {
        recordChars(current, cleanText);
      }
      i = plainEnd;
    }
  }
}

/**
 * Result of processing one override block. `font` includes active inline
 * overrides, while `style` is the baseline selected by the latest successful
 * `\rNamed` (or the event style after bare/unknown `\r`).
 */
interface OverrideResult {
  font: FontKey;
  style: FontKey;
  isDrawing: boolean;
}

/** Discriminated union of override-tag matches collected across all five
 *  tag-family regexes. `pos` is the match's byte index inside the block,
 *  used to sort tags into source order before applying. */
type OverrideTag =
  | { kind: "r"; pos: number; styleName: string }
  | { kind: "fn"; pos: number; family: string }
  | { kind: "b"; pos: number; value: string }
  | { kind: "i"; pos: number; value: string }
  | { kind: "p"; pos: number; value: string };

/**
 * Apply override tags from one `{ … }` block in source order. The style
 * baseline must be distinct from inline overrides because style-relative
 * `\fn`, `\b`, and `\i` resets use the style selected by the latest `\rNamed`,
 * not necessarily the dialogue's original style. Present nonnumeric `\b`/`\i`
 * arguments parse as zero instead of using the baseline.
 */
function applyOverrideTags(
  block: string,
  current: FontKey,
  activeStyle: FontKey,
  currentDrawing: boolean,
  eventStyle: FontKey,
  styleMap: Map<string, FontKey>
): OverrideResult {
  let font = { ...current };
  let style = { ...activeStyle };
  let isDrawing = currentDrawing;

  // Collect matches from each tag family into one position-tagged list.
  // `m.index` is reliably set for matchAll results per the JS spec —
  // the optional-typing of `RegExpExecArray.index` in lib.es5.d.ts is
  // a `.match()` non-global concern that doesn't apply to matchAll.
  const tags: OverrideTag[] = [];
  for (const m of block.matchAll(R_TAG_RE)) {
    tags.push({ kind: "r", pos: m.index!, styleName: m[1]! });
  }
  for (const m of block.matchAll(FN_TAG_RE)) {
    tags.push({ kind: "fn", pos: m.index!, family: m[1]! });
  }
  for (const m of block.matchAll(B_TAG_RE)) {
    tags.push({ kind: "b", pos: m.index!, value: m[1]! });
  }
  for (const m of block.matchAll(I_TAG_RE)) {
    tags.push({ kind: "i", pos: m.index!, value: m[1]! });
  }
  for (const m of block.matchAll(P_TAG_RE)) {
    tags.push({ kind: "p", pos: m.index!, value: m[1]! });
  }

  // Stable position sort — preserves insertion order on tie, though
  // ties between different tag families are structurally impossible
  // (all start with `\` followed by a distinguishing letter).
  tags.sort((a, b) => a.pos - b.pos);

  for (const tag of tags) {
    switch (tag.kind) {
      case "r": {
        const trimmedStyleName = trimTrailingTagSpaces(tag.styleName);
        const styleName = Array.from(trimmedStyleName).length <= 128 ? trimmedStyleName : "";
        const namedStyle = styleName ? styleMap.get(styleName) : undefined;
        style = { ...(namedStyle ?? eventStyle) };
        font = { ...style };
        // libass's `\r` render-context reset intentionally leaves
        // `drawing_scale` unchanged; only `\p` changes drawing mode.
        break;
      }
      case "fn": {
        // Capture first, sanitize second. If FN_TAG_RE treats spoofing
        // whitespace, BiDi, or astral codepoints as boundaries, malformed
        // names become prefix aliases (for example `Arial<NBSP>Black` ->
        // `Arial`). Let sanitizeFamily see the full family name. Bare `\fn`
        // and exact `\fn0` reset to the currently active style family.
        const familyArg = trimTrailingTagSpaces(tag.family);
        if (!familyArg || familyArg === "0") {
          font.family = style.family;
          break;
        }
        // libass checks exact `0` before skipping leading argument whitespace.
        // Apply the project length gate after that same leading trim.
        const familyForLength = familyArg.replace(/^[ \t]+/u, "");
        if (Array.from(familyForLength).length > 128) {
          font.family = style.family;
          break;
        }
        const rawFamily = normalizeFamily(familyArg);
        const sanitizedFamily = sanitizeFamily(rawFamily);
        if (!sanitizedFamily) {
          font.family = style.family;
        } else {
          font.family = sanitizedFamily;
        }
        break;
      }
      case "b": {
        const weight = parseTagInteger(tag.value);
        if (weight === null || !(weight === 0 || weight === 1 || weight >= 100)) {
          font.bold = style.bold;
        } else {
          // FontKey has a boolean bold axis, while libass preserves exact
          // weights >=100. Keep the existing regular/bold threshold without
          // claiming exact fidelity for intermediate weights.
          font.bold = weight === 1 || weight >= 700;
        }
        break;
      }
      case "i": {
        const flag = parseTagInteger(tag.value);
        font.italic = flag === 0 || flag === 1 ? flag === 1 : style.italic;
        break;
      }
      case "p": {
        const scale = parseTagInteger(tag.value);
        // Bare, invalid, and negative drawing scales resolve to zero.
        isDrawing = scale !== null && scale > 0;
        break;
      }
    }
  }

  return { font, style, isDrawing };
}

function trimTrailingTagSpaces(value: string): string {
  return value.replace(/[ \t]+$/u, "");
}

function parseTagInteger(value: string): number | null {
  const normalized = trimTrailingTagSpaces(value);
  if (!normalized) return null;
  const match = /^\s*[+-]?\d+/.exec(normalized);
  // libass's integer parser yields zero for a present but nonnumeric argument.
  if (!match) return 0;
  const parsed = Number.parseInt(match[0], 10);
  return Number.isNaN(parsed) ? 0 : parsed;
}

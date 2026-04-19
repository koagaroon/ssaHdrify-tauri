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

// Lazy dynamic import — only triggers when ensureLoaded() is first called.
// Previously this ran at module load time, which blocked startup after the
// CSS visibility refactor made all tabs mount immediately.
let parseFn: ((text: string) => ParsedASS) | null = null;
let assCompilerReady: Promise<void> | null = null;

export interface FontKey {
  family: string;
  bold: boolean;
  italic: boolean;
}

export interface FontUsage {
  key: FontKey;
  codepoints: Set<number>;
}

/**
 * Serialize a FontKey to a stable string for Map keys.
 */
function fontKeyToString(key: FontKey): string {
  return `${key.family}|${key.bold ? "B" : ""}${key.italic ? "I" : ""}`;
}

/** Format a FontKey as a human-readable label (e.g., "Arial Bold Italic"). */
export function fontKeyLabel(key: FontKey): string {
  let label = key.family;
  if (key.bold) label += " Bold";
  if (key.italic) label += " Italic";
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
 *  captured from a subtitle file before it flows into matching or output. */
function sanitizeFamily(raw: string): string {
  return (
    raw
      // eslint-disable-next-line no-control-regex -- intentional: sanitize control chars from subtitle font names
      .replace(/[\x00-\x1f\x7f]/g, "")
      .slice(0, 128)
  );
}

/**
 * Ensure ass-compiler is loaded. Call before using collector functions.
 */
export async function ensureLoaded(): Promise<void> {
  if (!assCompilerReady) {
    assCompilerReady = import("ass-compiler")
      .then((m) => {
        parseFn = m.parse;
      })
      .catch((e) => {
        assCompilerReady = null; // allow retry on next call
        throw e;
      });
  }
  await assCompilerReady;
}

/**
 * Collect font usage from an ASS file.
 *
 * @param assContent - Full ASS file content as string
 * @returns Array of FontUsage entries (unique per font family+style combo)
 */
export function collectFonts(assContent: string): FontUsage[] {
  if (!parseFn) {
    throw new Error("ASS compiler not loaded yet");
  }

  // Parse ASS file
  const parsed = parseFn(assContent);
  if (!parsed) {
    throw new Error("Failed to parse ASS file");
  }

  // Build style → font map from [V4+ Styles]
  const styleMap = new Map<string, { family: string; bold: boolean; italic: boolean }>();

  if (parsed.styles?.style) {
    for (const style of parsed.styles.style) {
      // Drop the ASS `@` vertical-writing prefix (collapses vertical and
      // horizontal uses into one entry), then strip control chars and cap length.
      const family = sanitizeFamily(normalizeFamily(style.Fontname || "Arial"));
      styleMap.set(style.Name, {
        family: family || "Arial",
        bold: parseInt(style.Bold || "0", 10) !== 0,
        italic: parseInt(style.Italic || "0", 10) !== 0,
      });
    }
  }

  // Accumulate: fontKeyString → { key, codepoints }
  const usageMap = new Map<string, FontUsage>();
  // Per-font caps are not enough on their own: 500 variants × 65,536 codepoints
  // each would reach ~130 MB of Set overhead before any single cap triggers.
  // Bound the combined codepoint count across all variants as defense-in-depth.
  const MAX_TOTAL_CODEPOINTS = 1_000_000;
  let totalCodepoints = 0;

  function recordChars(key: FontKey, text: string) {
    const keyStr = fontKeyToString(key);
    let usage = usageMap.get(keyStr);
    if (!usage) {
      usage = { key: { ...key }, codepoints: new Set() };
      usageMap.set(keyStr, usage);
      if (usageMap.size > 500) {
        throw new Error(`Too many font variants: ${usageMap.size} (max 500)`);
      }
    }
    // Upper-bound the per-variant codepoint set. 65536 is a defensive cap,
    // not the Basic Multilingual Plane boundary (which caps at U+FFFF). Real
    // fonts carry well under this; the cap only fires against crafted ASS
    // that enumerates tens of thousands of distinct characters for one font.
    const MAX_CODEPOINTS_PER_VARIANT = 65536;
    for (const char of text) {
      if (usage.codepoints.size >= MAX_CODEPOINTS_PER_VARIANT) {
        break;
      }
      const cp = char.codePointAt(0);
      if (cp !== undefined && cp > 32 && cp <= 0x10ffff) {
        // Skip control chars, space, and invalid codepoints
        const before = usage.codepoints.size;
        usage.codepoints.add(cp);
        if (usage.codepoints.size !== before) {
          totalCodepoints++;
          if (totalCodepoints > MAX_TOTAL_CODEPOINTS) {
            throw new Error(
              `Too many codepoints across fonts: ${totalCodepoints} (max ${MAX_TOTAL_CODEPOINTS})`
            );
          }
        }
      }
    }
  }

  if (parsed.events?.dialogue) {
    for (const dialogue of parsed.events.dialogue) {
      const styleName = dialogue.Style || "Default";
      const baseStyle: FontKey = styleMap.get(styleName) ?? {
        family: "Arial",
        bold: false,
        italic: false,
      };
      const rawText: string = dialogue.Text?.raw ?? "";
      processDialogueText(rawText, baseStyle, recordChars);
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
function processDialogueText(
  text: string,
  initialFont: FontKey,
  recordChars: (key: FontKey, text: string) => void
) {
  let current = { ...initialFont };
  let isDrawing = false;
  let i = 0;

  while (i < text.length) {
    if (text[i] === "{") {
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
          if (tail.length > 0) recordChars(current, tail);
        }
        return;
      }

      const block = text.slice(i + 1, closeIdx);
      current = applyOverrideTags(block, current, initialFont);
      // Reset drawing on \p0 or \r — checked independently from \p[1-9]
      // so that {\r\p1} correctly resets then re-enables drawing mode.
      // The \r anchor is `\r(?=\\|}|$|[A-Za-z])` so both `\rStyleName` and
      // lowercased `\rdefault` match, while made-up tokens like `\rnd`
      // (the only one rejected) do not trigger a false style reset.
      // (Technically `\rnd` would still match [A-Za-z] — but no real
      // ASS override starts with `\rn...`, so collisions are a non-issue.)
      if (/\\p0/.test(block) || /\\r(?=\\|}|$|[A-Za-z])/.test(block)) {
        isDrawing = false;
      }
      if (/\\p[1-9]/.test(block)) {
        isDrawing = true;
      }
      i = closeIdx + 1;
    } else {
      // Plain text — find the next override block or end
      const nextBrace = text.indexOf("{", i);
      const plainEnd = nextBrace >= 0 ? nextBrace : text.length;
      const plain = text.slice(i, plainEnd);

      // Skip ASS drawing commands (\N, \n, \h) and line breaks
      const cleanText = plain.replace(/\\N/g, "").replace(/\\n/g, "").replace(/\\h/g, "");

      if (cleanText.length > 0 && !isDrawing) {
        recordChars(current, cleanText);
      }
      i = plainEnd;
    }
  }
}

/**
 * Apply override tags from a single { ... } block to the current font state.
 */
function applyOverrideTags(block: string, current: FontKey, initialFont: FontKey): FontKey {
  let result = { ...current };

  // \r[StyleName] — reset to base style (but don't return early;
  // subsequent tags like \fn in the same block must still be applied).
  // Lookahead accepts `[A-Za-z]` so both `\rBoldStyle` and the
  // lowercase-named `\rdefault` match, matching the sibling check in
  // `processDialogueText` above. The anchor avoids matching made-up
  // tokens like the literal byte sequence `\r` + tailing text that
  // doesn't form a style reset.
  if (/\\r(?=\\|}|$|[A-Za-z])/.test(block)) {
    result = { ...initialFont };
  }

  // \fn<FontName> — change font family (empty \fn resets to style default).
  // The `@` vertical-writing prefix is a rendering hint, not part of the
  // font identity; strip it so `\fn@Foo` and `\fnFoo` resolve to the same
  // font file and share a FontUsage entry (codepoints merge for subsetting).
  // Cap the capture at 128 chars: `sanitizeFamily` will also slice to 128,
  // but bounding the match keeps allocator cost low against crafted ASS
  // with absurdly long names inside an override block.
  const fnMatch = block.match(/\\fn([^\\}]{0,128})/);
  if (fnMatch) {
    const rawFamily = normalizeFamily(fnMatch[1]);
    if (!rawFamily) {
      result.family = initialFont.family;
    } else {
      result.family = sanitizeFamily(rawFamily) || current.family;
    }
  }

  // \b<0|1|weight> — bold
  const bMatch = block.match(/\\b(\d+)/);
  if (bMatch) {
    const val = parseInt(bMatch[1], 10);
    // \b0 = not bold, \b1 = bold, \b700+ = bold by weight
    result.bold = val === 1 || val >= 700;
  }

  // \i<0|1> — italic
  const iMatch = block.match(/\\i(\d+)/);
  if (iMatch) {
    result.italic = parseInt(iMatch[1], 10) !== 0;
  }

  return result;
}

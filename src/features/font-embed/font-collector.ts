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

// Dynamic import for code-splitting; ass-compiler uses named exports
let parseFn: ((text: string) => ParsedASS) | null = null;
const assCompilerReady = import("ass-compiler").then((m) => {
  parseFn = m.parse;
});

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

/**
 * Ensure ass-compiler is loaded. Call before using collector functions.
 */
export async function ensureLoaded(): Promise<void> {
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
  const styleMap = new Map<
    string,
    { family: string; bold: boolean; italic: boolean }
  >();

  if (parsed.styles?.style) {
    for (const style of parsed.styles.style) {
      styleMap.set(style.Name, {
        family: style.Fontname || "Arial",
        bold: parseInt(style.Bold || "0") !== 0,
        italic: parseInt(style.Italic || "0") !== 0,
      });
    }
  }

  // Accumulate: fontKeyString → { key, codepoints }
  const usageMap = new Map<string, FontUsage>();

  function recordChars(key: FontKey, text: string) {
    const keyStr = fontKeyToString(key);
    let usage = usageMap.get(keyStr);
    if (!usage) {
      usage = { key: { ...key }, codepoints: new Set() };
      usageMap.set(keyStr, usage);
    }
    for (const char of text) {
      const cp = char.codePointAt(0);
      if (cp !== undefined && cp > 32) {
        // Skip control chars and space
        usage.codepoints.add(cp);
      }
    }
  }

  // Walk dialogue events
  if (parsed.events?.dialogue) {
    for (const dialogue of parsed.events.dialogue) {
      // Get base font from style
      const styleName = dialogue.Style || "Default";
      const baseStyle = styleMap.get(styleName) ?? {
        family: "Arial",
        bold: false,
        italic: false,
      };

      // Current active font state (starts with the line's style)
      const currentFont: FontKey = { ...baseStyle };

      // Parse the dialogue text for override tags and plain text
      const rawText: string = dialogue.Text?.raw ?? "";
      processDialogueText(rawText, currentFont, recordChars);
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
  let i = 0;

  while (i < text.length) {
    if (text[i] === "{") {
      // Override block — parse tags until closing }
      const closeIdx = text.indexOf("}", i);
      if (closeIdx === -1) break; // malformed, bail

      const block = text.slice(i + 1, closeIdx);
      current = applyOverrideTags(block, current);
      i = closeIdx + 1;
    } else {
      // Plain text — find the next override block or end
      const nextBrace = text.indexOf("{", i);
      const plainEnd = nextBrace >= 0 ? nextBrace : text.length;
      const plain = text.slice(i, plainEnd);

      // Skip ASS drawing commands (\N, \n, \h) and line breaks
      const cleanText = plain
        .replace(/\\N/g, "")
        .replace(/\\n/g, "")
        .replace(/\\h/g, "");

      if (cleanText.length > 0) {
        recordChars(current, cleanText);
      }
      i = plainEnd;
    }
  }
}

/**
 * Apply override tags from a single { ... } block to the current font state.
 */
function applyOverrideTags(block: string, current: FontKey): FontKey {
  const result = { ...current };

  // \fn<FontName> — change font family
  const fnMatch = block.match(/\\fn([^\\}]+)/);
  if (fnMatch) {
    result.family = fnMatch[1].trim();
  }

  // \b<0|1|weight> — bold
  const bMatch = block.match(/\\b(\d+)/);
  if (bMatch) {
    const val = parseInt(bMatch[1]);
    // \b0 = not bold, \b1 = bold, \b700+ = bold by weight
    result.bold = val === 1 || val >= 700;
  }

  // \i<0|1> — italic
  const iMatch = block.match(/\\i(\d+)/);
  if (iMatch) {
    result.italic = parseInt(iMatch[1]) !== 0;
  }

  return result;
}

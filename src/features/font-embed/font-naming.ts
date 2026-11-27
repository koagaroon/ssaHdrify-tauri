import type { FontKey } from "./font-collector";

/**
 * FNV-1a-inspired 32-bit hash over Unicode codepoints, rendered as 8 lowercase hex chars.
 *
 * Used to disambiguate embedded `[Fonts]` filenames when a font family name
 * contains non-ASCII characters (either pure CJK like "思源黑体" or mixed CJK +
 * ASCII like "思源黑体 CN" vs "思源宋体 CN") that the ASCII filename sanitizer
 * strips. Iterating over full Unicode codepoints avoids splitting astral
 * surrogate pairs across independent hash rounds.
 */
export function familyStableHash(family: string): string {
  let h = 0x811c9dc5;
  for (const ch of family) {
    const cp = ch.codePointAt(0) ?? 0;
    h ^= cp & 0xff;
    h = Math.imul(h, 0x01000193);
    h ^= (cp >>> 8) & 0xff;
    h = Math.imul(h, 0x01000193);
    h ^= (cp >>> 16) & 0xff;
    h = Math.imul(h, 0x01000193);
  }
  return (h >>> 0).toString(16).padStart(8, "0");
}

/**
 * Build the font name for an ASS `[Fonts]` `fontname:` header.
 * Convention: `family_bold_italic.ttf` (all lowercase).
 *
 * Pure printable-ASCII family names keep their clean human-readable stem
 * (`Arial` -> `arial.ttf`). Families containing non-ASCII characters append
 * `familyStableHash(key.family)` so mixed names like `"思源黑体 CN"` and
 * `"思源宋体 CN"` cannot collapse onto the same `cn.ttf` header.
 */
export function buildFontFileName(key: FontKey): string {
  let name = key.family
    .toLowerCase()
    .replace(/[^a-z0-9_-]/g, "_")
    .replace(/_+/g, "_")
    .replace(/^_|_$/g, "");
  if (!name) {
    name = `font_${familyStableHash(key.family)}`;
  } else if (/[^\u0020-\u007e]/u.test(key.family)) {
    name = `${name}_${familyStableHash(key.family)}`;
  }
  if (key.bold) name += "_bold";
  if (key.italic) name += "_italic";
  return `${name}.ttf`;
}

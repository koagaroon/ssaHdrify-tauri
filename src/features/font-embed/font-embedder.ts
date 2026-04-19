/**
 * Font embedder — orchestrate the full pipeline:
 * 1. Collect font usage from ASS file (font-collector)
 * 2. Resolve font files via Rust (find_system_font)
 * 3. Subset fonts to only used glyphs (subset-font)
 * 4. Encode subsets in ASS UUEncode format (ass-uuencode)
 * 5. Insert [Fonts] section into ASS file
 */
import {
  collectFonts,
  ensureLoaded as ensureCollectorLoaded,
  fontKeyLabel,
  type FontUsage,
  type FontKey,
} from "./font-collector";
import { buildFontEntry } from "./ass-uuencode";
import { findSystemFont, subsetFont, type LocalFontEntry } from "../../lib/tauri-api";
import { SECTION_HEADER_RE } from "../hdr-convert/ass-processor";

// ── Types ─────────────────────────────────────────────────

/** Where a resolved font came from. Shown as a badge in the main font list.
 *  Named `FontProvenance` (not `FontSource`) to avoid colliding with
 *  `FontSource` in FontSourceModal, which is a struct describing a
 *  user-picked source (folder or file set). */
export type FontProvenance = "local" | "system";

export interface FontInfo {
  key: FontKey;
  glyphCount: number;
  filePath: string | null;
  /** Face index within the font file (0 for TTF, may be >0 for TTC) */
  fontIndex: number;
  error: string | null;
  /** null when the font could not be resolved */
  source: FontProvenance | null;
}

/**
 * Map key for the user font map: "family|bold|italic" with family lowercased.
 * Kept as a plain string so React state can memo-compare equality cheaply and
 * so the same key derivation is trivially reproducible in the UI layer.
 */
export function userFontKey(family: string, bold: boolean, italic: boolean): string {
  return `${family.toLowerCase()}|${bold ? "1" : "0"}|${italic ? "1" : "0"}`;
}

export interface EmbedProgress {
  stage: string;
  current: number;
  total: number;
}

// ── Font Discovery ────────────────────────────────────────

/**
 * Build a lookup map from a list of local font faces. One face contributes
 * one map entry **per family-name variant** it carries — so a CJK font with
 * both English and Chinese names in its name table becomes two keys that
 * both resolve to the same (path, index) tuple. This is what lets an ASS
 * script's Fontname match the font no matter which language the typesetter
 * chose to reference.
 */
export function buildUserFontMap(faces: LocalFontEntry[]): Map<string, LocalFontEntry> {
  const map = new Map<string, LocalFontEntry>();
  for (const face of faces) {
    for (const family of face.families) {
      map.set(userFontKey(family, face.bold, face.italic), face);
    }
  }
  return map;
}

/**
 * Analyze an ASS file: collect fonts and resolve each font to either a
 * user-provided local font (preferred) or a system-installed font.
 *
 * @param assContent - Full ASS file content
 * @param userFontMap - Optional map keyed by `userFontKey(family, bold, italic)`.
 *                     Build with `buildUserFontMap()`. When present, entries
 *                     override system matches so that user-supplied fonts
 *                     always win.
 */
export async function analyzeFonts(
  assContent: string,
  userFontMap?: Map<string, LocalFontEntry>
): Promise<{ infos: FontInfo[]; usages: FontUsage[] }> {
  await ensureCollectorLoaded();

  const usages = collectFonts(assContent);
  const infos: FontInfo[] = [];

  // Development diagnostic: when a user reports "font X doesn't match", the
  // only way to confirm from logs is to see both the lookup key and every
  // key actually present in the map. Gated on DEV because font paths in the
  // console would leak the user's machine layout in any shared screenshot.
  const isDev = import.meta.env.DEV;
  if (isDev && userFontMap && userFontMap.size > 0) {
    const sample = Array.from(userFontMap.keys()).slice(0, 20);
    console.debug(
      `[ssaHdrify] userFontMap has ${userFontMap.size} keys; first ${sample.length}:`,
      sample
    );
  }

  for (const usage of usages) {
    const key = userFontKey(usage.key.family, usage.key.bold, usage.key.italic);
    const base = { key: usage.key, glyphCount: usage.codepoints.size };

    const local = userFontMap?.get(key);
    if (local) {
      if (isDev) console.debug(`[ssaHdrify] '${usage.key.family}' → LOCAL ${local.path}`);
      infos.push({
        ...base,
        filePath: local.path,
        fontIndex: local.index,
        error: null,
        source: "local",
      });
      continue;
    }

    try {
      const result = await findSystemFont(usage.key.family, usage.key.bold, usage.key.italic);
      if (isDev) console.debug(`[ssaHdrify] '${usage.key.family}' → SYSTEM ${result.path}`);
      infos.push({
        ...base,
        filePath: result.path,
        fontIndex: result.index,
        error: null,
        source: "system",
      });
    } catch (e) {
      const reason = e instanceof Error ? e.message : String(e);
      if (isDev) {
        console.debug(`[ssaHdrify] '${usage.key.family}' → MISS (key='${key}', reason=${reason})`);
      }
      infos.push({
        ...base,
        filePath: null,
        fontIndex: 0,
        error: reason,
        source: null,
      });
    }
  }

  return { infos, usages };
}

/**
 * FNV-1a 32-bit hash, rendered as 8 lowercase hex chars. Used only as a
 * filename suffix for fonts whose family name is entirely non-ASCII (typical
 * for CJK fonts like "思源黑体"). Without the hash two different CJK fonts
 * would both collapse to `font.ttf` and collide inside the embedded [Fonts]
 * section — the renderer would then load whichever face hit the parser
 * first, producing visual corruption that looks like a random font swap.
 *
 * Iterates over full codepoints (via `for...of`) rather than UTF-16 code
 * units so that astral-plane CJK (Extension-B+) and emoji don't hash their
 * surrogate halves independently — two fonts that differ only in the
 * astral half could otherwise collide.
 */
function familyFnvHash(family: string): string {
  let h = 0x811c9dc5;
  for (const ch of family) {
    const cp = ch.codePointAt(0) ?? 0;
    // Mix each byte of the 32-bit codepoint into the hash so astral-plane
    // codepoints (U+10000..U+10FFFF) contribute all their bits.
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
 * Build the font name for the [Fonts] section entry.
 * Convention: family_bold_italic.ttf (all lowercase)
 */
function buildFontFileName(key: FontKey): string {
  let name = key.family
    .toLowerCase()
    .replace(/[^a-z0-9_-]/g, "_") // strip everything except safe chars
    .replace(/_+/g, "_") // collapse consecutive underscores
    .replace(/^_|_$/g, ""); // trim leading/trailing underscores
  // When the ASCII-only strip empties the name — common for pure-CJK family
  // names — append a stable hash of the original so distinct CJK fonts don't
  // collide on the same `font.ttf` filename inside the [Fonts] section.
  if (!name) name = `font_${familyFnvHash(key.family)}`;
  if (key.bold) name += "_bold";
  if (key.italic) name += "_italic";
  return `${name}.ttf`;
}

/**
 * Embed selected fonts into an ASS file.
 *
 * @param assContent - Original ASS file content
 * @param selectedFonts - Font infos to embed (must have valid filePaths)
 * @param fontUsages - Full font usage data (for codepoint sets)
 * @param onProgress - Optional progress callback
 * @returns Modified ASS content with [Fonts] section
 */
export async function embedFonts(
  assContent: string,
  selectedFonts: FontInfo[],
  fontUsages: FontUsage[],
  onProgress?: (progress: EmbedProgress) => void,
  isCancelled?: () => boolean,
  t?: (key: string, ...args: (string | number)[]) => string
): Promise<{ content: string; embeddedCount: number } | null> {
  const total = selectedFonts.length;
  const fontEntries: string[] = [];

  for (let i = 0; i < selectedFonts.length; i++) {
    if (isCancelled?.()) break;

    const info = selectedFonts[i];
    if (!info.filePath) continue;

    const fontName = buildFontFileName(info.key);
    const label = fontKeyLabel(info.key);
    onProgress?.({
      stage: t?.("msg_subsetting", label) ?? `Subsetting ${label}...`,
      current: i + 1,
      total,
    });

    // Find the matching usage to get codepoints
    const usage = fontUsages.find(
      (u) =>
        u.key.family === info.key.family &&
        u.key.bold === info.key.bold &&
        u.key.italic === info.key.italic
    );
    if (!usage) {
      // Selected FontInfo has no matching FontUsage — means analyzeFonts and
      // the current fontUsages array disagree, which should be impossible if
      // both came from the same ASS parse. Log so the drift is debuggable
      // instead of silently producing an embed file missing this font.
      console.warn(`[ssaHdrify] embedFonts: no usage entry for ${fontKeyLabel(info.key)}`);
      continue;
    }

    // Subset the font to only used glyphs (via Rust backend)
    let subsetData: Uint8Array;
    try {
      subsetData = await subsetFont(info.filePath, info.fontIndex, Array.from(usage.codepoints));
      if (isCancelled?.()) return null;
    } catch (subsetErr) {
      console.warn(`Font subsetting failed for ${usage.key.family}, skipping: ${subsetErr}`);
      onProgress?.({
        stage:
          t?.("msg_font_skipped", info.key.family, String(subsetErr)) ??
          `Skipped ${info.key.family}: ${subsetErr}`,
        current: i + 1,
        total,
      });
      continue; // Skip this font, don't fall back to unguarded read
    }

    // Build the [Fonts] entry
    fontEntries.push(buildFontEntry(fontName, subsetData));
  }

  if (isCancelled?.()) {
    return null;
  }

  if (fontEntries.length === 0) {
    return { content: assContent, embeddedCount: 0 };
  }

  // Build [Fonts] section (no leading \n — insertFontsSection handles the separator)
  const fontsSection = `[Fonts]\n${fontEntries.join("\n\n")}\n`;

  // Insert [Fonts] section into ASS file
  return {
    content: insertFontsSection(assContent, fontsSection),
    embeddedCount: fontEntries.length,
  };
}

/**
 * Insert [Fonts] section into ASS content.
 * Position: after [V4+ Styles], before [Events].
 * If [Fonts] already exists, replace it.
 */
function insertFontsSection(content: string, fontsSection: string): string {
  const lineEnding = content.includes("\r\n") ? "\r\n" : "\n";
  const lines = content.split(/\r?\n/);

  // Adapt fontsSection to match the file's line ending
  const adaptedFontsSection = fontsSection.replace(/\n/g, lineEnding);

  // Check if [Fonts] section already exists. The match is anchored at
  // column 0 and only tolerates trailing whitespace — a UUEncode data
  // line that happens to lowercase to `[fonts]` would never start at
  // column 0 with `[` being the first byte and nothing but whitespace
  // after `]`, because the 6-bit alphabet (33–96) does not include
  // space (32). This closes the false-positive hole that a looser
  // `.trim().toLowerCase()` comparison left open.
  const HEADER_FONTS_RE = /^\[[Ff][Oo][Nn][Tt][Ss]\]\s*$/;
  const existingFontsIdx = lines.findIndex((l) => HEADER_FONTS_RE.test(l));

  // Build "before" from a line slice: strip trailing blank lines so we control
  // the separator ourselves. Array.join() absorbs trailing "" elements into a
  // single lineEnding, making blank separator lines invisible — so we strip them
  // and add an explicit blank-line separator instead.
  const buildBefore = (endIdx: number): { text: string; sep: string } => {
    const slice = lines.slice(0, endIdx);
    while (slice.length > 0 && slice[slice.length - 1].trim() === "") {
      slice.pop();
    }
    const text = slice.join(lineEnding);
    // One blank line separator when there is content before; nothing when [Fonts] is at start
    const sep = slice.length > 0 ? lineEnding + lineEnding : "";
    return { text, sep };
  };

  // Build "after" from a line slice: strip leading blank lines so section
  // separators are normalized to exactly one blank line. This is intentional —
  // ASS convention is one blank line between sections. Files with 2+ blank
  // lines between sections (from manual editing or other tools) are normalized
  // on output. Without this stripping, blank lines between an old [Fonts]
  // block and the next section header would leak through as extra blank lines.
  const buildAfter = (startIdx: number): string => {
    const slice = lines.slice(startIdx);
    while (slice.length > 0 && slice[0].trim() === "") {
      slice.shift();
    }
    return slice.join(lineEnding);
  };

  // Lowercase before testing: SECTION_HEADER_RE's lookahead requires [a-z ],
  // which fails on all-uppercase headers like [EVENTS] if not lowercased.
  const isSectionHeader = (line: string) => SECTION_HEADER_RE.test(line.trim().toLowerCase());

  if (existingFontsIdx >= 0) {
    // Find the end of the existing [Fonts] section (next section header or EOF).
    let endIdx = existingFontsIdx + 1;
    while (endIdx < lines.length && !isSectionHeader(lines[endIdx])) {
      endIdx++;
    }

    const { text: before, sep } = buildBefore(existingFontsIdx);
    const after = buildAfter(endIdx);
    // Only add separator before after when there IS content after [Fonts].
    // When [Fonts] is the last section, after is "" and adaptedFontsSection
    // already ends with lineEnding — adding another would create a trailing blank.
    const afterSep = after.length > 0 ? lineEnding : "";
    return `${before}${sep}${adaptedFontsSection}${afterSep}${after}`;
  }

  // No existing [Fonts] — insert before [Events]. Same column-0 strict
  // match as above for the same UUEncode-false-positive reason.
  const HEADER_EVENTS_RE = /^\[[Ee][Vv][Ee][Nn][Tt][Ss]\]\s*$/;
  const eventsIdx = lines.findIndex((l) => HEADER_EVENTS_RE.test(l));

  if (eventsIdx >= 0) {
    const { text: before, sep } = buildBefore(eventsIdx);
    const after = lines.slice(eventsIdx).join(lineEnding);
    return `${before}${sep}${adaptedFontsSection}${lineEnding}${after}`;
  }

  // No [Events] section found — append at end with a blank line separator.
  // Strip trailing newlines from content to avoid double blank line.
  const trimmedContent = content.replace(/(\r\n|\n)+$/, "");
  return `${trimmedContent}${lineEnding}${lineEnding}${adaptedFontsSection}`;
}

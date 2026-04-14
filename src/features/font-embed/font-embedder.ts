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
  type FontUsage,
  type FontKey,
} from "./font-collector";
import { buildFontEntry } from "./ass-uuencode";
import { findSystemFont, readBinary } from "../../lib/tauri-api";
import { invoke } from "@tauri-apps/api/core";

// ── Types ─────────────────────────────────────────────────

export interface FontInfo {
  key: FontKey;
  glyphCount: number;
  filePath: string | null;
  error: string | null;
}

export interface EmbedProgress {
  stage: string;
  current: number;
  total: number;
}

// ── Font Discovery ────────────────────────────────────────

/**
 * Analyze an ASS file: collect fonts and resolve their system paths.
 *
 * @param assContent - Full ASS file content
 * @returns Array of FontInfo with resolved paths or errors
 */
export async function analyzeFonts(assContent: string): Promise<FontInfo[]> {
  await ensureCollectorLoaded();

  const usages = collectFonts(assContent);
  const results: FontInfo[] = [];

  for (const usage of usages) {
    try {
      const filePath = await findSystemFont(
        usage.key.family,
        usage.key.bold,
        usage.key.italic
      );
      results.push({
        key: usage.key,
        glyphCount: usage.codepoints.size,
        filePath,
        error: null,
      });
    } catch (e) {
      results.push({
        key: usage.key,
        glyphCount: usage.codepoints.size,
        filePath: null,
        error: e instanceof Error ? e.message : String(e),
      });
    }
  }

  return results;
}

/**
 * Build the font name for the [Fonts] section entry.
 * Convention: family_bold_italic.ttf (all lowercase)
 */
function buildFontFileName(key: FontKey): string {
  let name = key.family.toLowerCase().replace(/\s+/g, "_");
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
  onProgress?: (progress: EmbedProgress) => void
): Promise<string> {
  const total = selectedFonts.length;
  const fontEntries: string[] = [];

  for (let i = 0; i < selectedFonts.length; i++) {
    const info = selectedFonts[i];
    if (!info.filePath) continue;

    const fontName = buildFontFileName(info.key);
    onProgress?.({
      stage: `Subsetting ${info.key.family}${info.key.bold ? " Bold" : ""}${info.key.italic ? " Italic" : ""}`,
      current: i + 1,
      total,
    });

    // Read the full font file
    const fontData = await readBinary(info.filePath);

    // Find the matching usage to get codepoints
    const usage = fontUsages.find(
      (u) =>
        u.key.family === info.key.family &&
        u.key.bold === info.key.bold &&
        u.key.italic === info.key.italic
    );
    if (!usage) continue;

    // Subset the font to only used glyphs (via Rust backend)
    let subsetData: Uint8Array;
    try {
      const codepoints = Array.from(usage.codepoints);
      const subsetBytes: number[] = await invoke("subset_font", {
        fontPath: info.filePath,
        codepoints,
      });
      subsetData = new Uint8Array(subsetBytes);
    } catch {
      // If subsetting fails (e.g., not implemented yet), use full font
      subsetData = fontData;
    }

    // Build the [Fonts] entry
    fontEntries.push(buildFontEntry(fontName, subsetData));
  }

  if (fontEntries.length === 0) {
    return assContent;
  }

  // Build [Fonts] section
  const fontsSection = `\n[Fonts]\n${fontEntries.join("\n\n")}\n`;

  // Insert [Fonts] section into ASS file
  return insertFontsSection(assContent, fontsSection);
}

/**
 * Insert [Fonts] section into ASS content.
 * Position: after [V4+ Styles], before [Events].
 * If [Fonts] already exists, replace it.
 */
function insertFontsSection(content: string, fontsSection: string): string {
  const lines = content.split(/\r?\n/);

  // Check if [Fonts] section already exists
  const existingFontsIdx = lines.findIndex(
    (l) => l.trim().toLowerCase() === "[fonts]"
  );

  if (existingFontsIdx >= 0) {
    // Find the end of the existing [Fonts] section (next section header)
    let endIdx = existingFontsIdx + 1;
    while (endIdx < lines.length) {
      if (lines[endIdx].trim().startsWith("[")) break;
      endIdx++;
    }
    // Replace existing [Fonts] section
    const before = lines.slice(0, existingFontsIdx).join("\n");
    const after = lines.slice(endIdx).join("\n");
    return `${before}${fontsSection}\n${after}`;
  }

  // No existing [Fonts] — insert before [Events]
  const eventsIdx = lines.findIndex(
    (l) => l.trim().toLowerCase() === "[events]"
  );

  if (eventsIdx >= 0) {
    const before = lines.slice(0, eventsIdx).join("\n");
    const after = lines.slice(eventsIdx).join("\n");
    return `${before}${fontsSection}\n${after}`;
  }

  // No [Events] section found — append at end
  return content + fontsSection;
}

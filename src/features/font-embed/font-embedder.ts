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
import { findSystemFont, subsetFont } from "../../lib/tauri-api";

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
  let name = key.family
    .toLowerCase()
    .replace(/[^a-z0-9_-]/g, "_")  // strip everything except safe chars
    .replace(/_+/g, "_")           // collapse consecutive underscores
    .replace(/^_|_$/g, "");        // trim leading/trailing underscores
  if (!name) name = "font";       // fallback if name becomes empty
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
    const fontLabel = `${info.key.family}${info.key.bold ? " Bold" : ""}${info.key.italic ? " Italic" : ""}`;
    onProgress?.({
      stage: t?.("msg_subsetting", fontLabel) ?? `Subsetting ${fontLabel}...`,
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
    if (!usage) continue;

    // Subset the font to only used glyphs (via Rust backend)
    let subsetData: Uint8Array;
    try {
      subsetData = await subsetFont(info.filePath, Array.from(usage.codepoints));
      if (isCancelled?.()) return null;
    } catch (subsetErr) {
      console.warn(`Font subsetting failed for ${usage.key.family}, skipping: ${subsetErr}`);
      onProgress?.({
        stage: t?.("msg_font_skipped", info.key.family, String(subsetErr)) ?? `Skipped ${info.key.family}: ${subsetErr}`,
        current: i + 1,
        total,
      });
      continue;  // Skip this font, don't fall back to unguarded read
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

  // Build [Fonts] section
  const fontsSection = `\n[Fonts]\n${fontEntries.join("\n\n")}\n`;

  // Insert [Fonts] section into ASS file
  return { content: insertFontsSection(assContent, fontsSection), embeddedCount: fontEntries.length };
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
    const before = lines.slice(0, existingFontsIdx).join(lineEnding);
    const after = lines.slice(endIdx).join(lineEnding);
    return `${before}${adaptedFontsSection}${lineEnding}${after}`;
  }

  // No existing [Fonts] — insert before [Events]
  const eventsIdx = lines.findIndex(
    (l) => l.trim().toLowerCase() === "[events]"
  );

  if (eventsIdx >= 0) {
    const before = lines.slice(0, eventsIdx).join(lineEnding);
    const after = lines.slice(eventsIdx).join(lineEnding);
    return `${before}${adaptedFontsSection}${lineEnding}${after}`;
  }

  // No [Events] section found — append at end
  return content + adaptedFontsSection;
}

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
import {
  findSystemFont,
  resolveUserFont,
  subsetFont,
  type LocalFontEntry,
} from "../../lib/tauri-api";
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

/** Resolution-only fields for a system-font lookup — everything in
 *  FontInfo except the per-file `key` and `glyphCount`. Used as the
 *  value type of the system-font cache so a single (family, bold,
 *  italic) lookup can be reused across every file in a batch. */
export type SystemFontResolution = Pick<FontInfo, "filePath" | "fontIndex" | "error" | "source">;

/**
 * Map key for the user font map: family + bold + italic joined by U+001F
 * (Unit Separator). Family is lowercased.
 *
 * The separator is a control character that real font family names never
 * contain (would be a malformed font / ASS file). The earlier `|`
 * separator could collide if a hostile or malformed family name itself
 * contained `|` — low severity, but switching to U+001F sidesteps the
 * question entirely. Pure string format change; no caller introspects
 * the key shape, so the swap is opaque to consumers.
 *
 * Plain string so React state can memo-compare equality cheaply and the
 * same derivation is trivially reproducible in the UI layer.
 */
const USER_FONT_KEY_SEP = "";
export function userFontKey(family: string, bold: boolean, italic: boolean): string {
  // NFC-normalize before lowercase so macOS HFS+ NFD-form filenames
  // and NFC-form font internal names key identically. Without this,
  // precomposed `é` (U+00E9) vs decomposed `e + ´` (U+0065+U+0301)
  // produce different keys for the same visual family — embedFonts
  // then silently mismatches its usage record at lookup time.
  const normalized = family.normalize("NFC").toLowerCase();
  return `${normalized}${USER_FONT_KEY_SEP}${bold ? "1" : "0"}${USER_FONT_KEY_SEP}${italic ? "1" : "0"}`;
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
 *
 * Production now resolves scanned sources through Rust's session-local index.
 * This helper remains for focused tests and small in-memory callers that
 * already have trusted `LocalFontEntry` objects.
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
 *                     override system matches so that tests and small
 *                     in-memory callers can inject local fonts directly.
 * @param useRustUserFonts - Production path: ask Rust's session-local source
 *                           index for a match before falling back to system.
 */
export async function analyzeFonts(
  assContent: string,
  userFontMap?: Map<string, LocalFontEntry> | null,
  systemFontCache?: Map<string, SystemFontResolution>,
  useRustUserFonts = false
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

    if (useRustUserFonts) {
      const localResult = await resolveUserFont(usage.key.family, usage.key.bold, usage.key.italic);
      if (localResult) {
        if (isDev) console.debug(`[ssaHdrify] '${usage.key.family}' → LOCAL ${localResult.path}`);
        infos.push({
          ...base,
          filePath: localResult.path,
          fontIndex: localResult.index,
          error: null,
          source: "local",
        });
        continue;
      }
    }

    // System lookup — first check the optional batch-shared cache so the
    // same (family, bold, italic) doesn't trigger N findSystemFont IPC
    // calls for an N-file batch. Cache holds both successful matches AND
    // misses; reusing a known miss avoids re-running a guaranteed-failing
    // lookup on every subsequent file that references the same font.
    const cached = systemFontCache?.get(key);
    if (cached) {
      infos.push({ ...base, ...cached });
      continue;
    }

    try {
      const result = await findSystemFont(usage.key.family, usage.key.bold, usage.key.italic);
      if (isDev) console.debug(`[ssaHdrify] '${usage.key.family}' → SYSTEM ${result.path}`);
      const resolution: SystemFontResolution = {
        filePath: result.path,
        fontIndex: result.index,
        error: null,
        source: "system",
      };
      systemFontCache?.set(key, resolution);
      infos.push({ ...base, ...resolution });
    } catch (e) {
      const reason = e instanceof Error ? e.message : String(e);
      if (isDev) {
        console.debug(`[ssaHdrify] '${usage.key.family}' → MISS (key='${key}', reason=${reason})`);
      }
      const resolution: SystemFontResolution = {
        filePath: null,
        fontIndex: 0,
        error: reason,
        source: null,
      };
      systemFontCache?.set(key, resolution);
      infos.push({ ...base, ...resolution });
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
 *
 * The `.ttf` suffix is unconditional even for OTF/TTC/OTC sources. The
 * extension inside the [Fonts] header is cosmetic — ASS renderers
 * (libass, VSFilter) identify embedded faces by the `fontname:` header,
 * not the filename extension, and accept TTF/OTF/CFF face data
 * regardless of the suffix. Keeping a single suffix avoids per-source
 * branching and a future maintainer "fixing" the literal to track the
 * source extension would produce noise for zero behavior change.
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

/** Per-file analysis cache entry — content stays in memory so the
 *  detection grid + aggregate can re-render on font-source changes
 *  without re-reading from disk, and the embed loop can reuse the
 *  already-resolved per-file (infos, usages) without a second pass. */
export interface FileAnalysis {
  content: string;
  infos: FontInfo[];
  usages: FontUsage[];
}

/**
 * Aggregate per-file analyses into a unified font list.
 *
 * Used by the batch detection grid: the user sees ONE row per unique
 * `(family, bold, italic)` triple referenced anywhere in the batch,
 * with `glyphCount` reflecting the UNION of codepoints across every
 * file that uses that font. Source / status comes from any file's
 * resolution (all files were analyzed against the same local-font resolver,
 * so the resolution is identical) — first occurrence wins.
 *
 * The returned `usages` carries the same union codepoints, suitable
 * for the FontSourceModal's batch-wide coverage stats. The per-file
 * `usages` from the cache stay authoritative for the embed loop's
 * per-file subsetting (each file embeds only the codepoints IT uses).
 */
export function aggregateFonts(perFile: Map<string, FileAnalysis>): {
  infos: FontInfo[];
  usages: FontUsage[];
} {
  // Union codepoints per font key across all files.
  const usageMap = new Map<string, { key: FontKey; codepoints: Set<number> }>();
  for (const analysis of perFile.values()) {
    for (const u of analysis.usages) {
      const k = userFontKey(u.key.family, u.key.bold, u.key.italic);
      const existing = usageMap.get(k);
      if (existing) {
        for (const cp of u.codepoints) existing.codepoints.add(cp);
      } else {
        usageMap.set(k, { key: u.key, codepoints: new Set(u.codepoints) });
      }
    }
  }

  // Resolution map — first occurrence per key. All per-file infos came
  // from the same local-font resolver, so source/status are consistent across
  // files; we just need ONE FontInfo per key as the row template.
  const infoTemplate = new Map<string, FontInfo>();
  for (const analysis of perFile.values()) {
    for (const info of analysis.infos) {
      const k = userFontKey(info.key.family, info.key.bold, info.key.italic);
      if (!infoTemplate.has(k)) {
        infoTemplate.set(k, info);
      }
    }
  }

  const aggInfos: FontInfo[] = [];
  const aggUsages: FontUsage[] = [];
  for (const [key, usage] of usageMap) {
    const tmpl = infoTemplate.get(key);
    if (!tmpl) continue;
    aggInfos.push({
      ...tmpl,
      glyphCount: usage.codepoints.size,
    });
    aggUsages.push({ key: usage.key, codepoints: usage.codepoints });
  }
  return { infos: aggInfos, usages: aggUsages };
}

/**
 * Derive the `.embedded.ass` output path for a given input ASS path.
 *
 * Used by the batch embed flow — Font Embed writes outputs alongside
 * inputs with a `.embedded` infix and a normalized `.ass` extension
 * (`EP01.ass` → `EP01.embedded.ass`, `EP01.ssa` → `EP01.embedded.ass`).
 * The native separator of the input path is preserved so the result
 * round-trips through Win32 APIs and shell-integration tools without
 * mixing slashes.
 *
 * Why a derived path instead of a per-file native save dialog: those
 * dialogs are blocking and don't scale to N files. The same-directory
 * convention matches the most common workflow and gives the user a
 * single overwrite-confirm gate via `countExistingFiles` before the
 * batch begins.
 */
export function deriveEmbeddedPath(inputPath: string): string {
  // Prefer backslash on output if the input has ANY backslash (Windows
  // path), regardless of whether it also contains a forward slash.
  // Mixed-separator inputs (e.g., a Windows path that picked up a `/`
  // from JS-side normalization upstream) used to bias to forward slash
  // because the heuristic only checked for "all backslash, no forward
  // slash" — which produced surprising `/`-formatted paths on Windows
  // when the rest of the system spoke `\`.
  const usedBackslash = inputPath.includes("\\");
  const normalized = inputPath.replace(/\\/g, "/");
  const lastSlash = normalized.lastIndexOf("/");
  const dir = lastSlash >= 0 ? normalized.slice(0, lastSlash) : "";
  const fullName = lastSlash >= 0 ? normalized.slice(lastSlash + 1) : normalized;
  const lastDot = fullName.lastIndexOf(".");
  const baseName = lastDot > 0 ? fullName.slice(0, lastDot) : fullName;
  // Output is always .ass — the embed step rebuilds an ASS-format file
  // regardless of whether the input was .ass or .ssa.
  const outputName = `${baseName}.embedded.ass`;
  const outputPath = dir ? `${dir}/${outputName}` : outputName;
  return usedBackslash ? outputPath.replace(/\//g, "\\") : outputPath;
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

  // Index fontUsages by lookup key once (O(N)) instead of linear-scanning
  // for each selected font (O(N²)). For a 40-font subtitle the scan cost
  // is negligible, but the index also makes the lookup intent explicit
  // and matches how analyzeFonts builds its own keyed maps.
  const usageByKey = new Map<string, FontUsage>();
  for (const u of fontUsages) {
    usageByKey.set(userFontKey(u.key.family, u.key.bold, u.key.italic), u);
  }

  for (let i = 0; i < selectedFonts.length; i++) {
    // Cancel between fonts — `break` here lets the post-loop
    // `if (isCancelled) return null` decide the final outcome,
    // matching the in-subset cancel below which DOES `return null`
    // directly (subset state is fully discarded mid-call). The
    // asymmetry is deliberate: between-font cancel preserves the
    // partial fontEntries built so far for inspection in the
    // post-loop fall-through (today they're discarded too, but
    // future "save what you have" flows would key on this shape).
    if (isCancelled?.()) break;

    const info = selectedFonts[i];
    if (!info.filePath) continue;

    const fontName = buildFontFileName(info.key);
    const label = fontKeyLabel(info.key);
    onProgress?.({
      stage: t?.("msg_subsetting", label) ?? `Subsetting ${label}…`,
      current: i + 1,
      total,
    });

    const usage = usageByKey.get(userFontKey(info.key.family, info.key.bold, info.key.italic));
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
  // Normalize Unicode line separators (U+2028 LINE SEPARATOR,
  // U+2029 PARAGRAPH SEPARATOR) to ASCII newlines BEFORE the split.
  // Without this, an ASS using U+2028 between sections collapses to
  // one giant line under `split(/\r?\n/)` — the column-0 [Fonts]
  // header regex can't match (header is now mid-line) and the new
  // section gets appended at end-of-file even though one already
  // exists. srt-converter does the same strip upstream; doing it
  // here keeps the section-rewrite path safe for direct callers
  // that bypass that converter.
  const normalized = content.replace(/[\u2028\u2029]/g, "\n");
  const lines = normalized.split(/\r?\n/);

  // Adapt fontsSection to match the file's line ending
  const adaptedFontsSection = fontsSection.replace(/\n/g, lineEnding);

  // Check if [Fonts] section already exists. Anchored at column 0 and
  // trailing whitespace restricted to ASCII space/tab only — plain `\s*`
  // would also match U+2028 / U+2029, letting a crafted ASS with
  // `[FONTS]\u2028` on one line still match the header regex. This
  // closes the false-positive hole that `.trim().toLowerCase()` left
  // open AND blocks the Unicode-line-sep smuggle.
  const HEADER_FONTS_RE = /^\[[Ff][Oo][Nn][Tt][Ss]\][ \t]*$/;
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

  // No existing [Fonts] — insert before [Events]. Same ASCII-whitespace
  // trailing match as above for the same UUEncode-false-positive +
  // Unicode-line-sep smuggling reason.
  const HEADER_EVENTS_RE = /^\[[Ee][Vv][Ee][Nn][Tt][Ss]\][ \t]*$/;
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

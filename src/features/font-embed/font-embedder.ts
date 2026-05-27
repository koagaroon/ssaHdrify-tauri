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
  lookupFontFamily,
  resolveUserFont,
  subsetFont,
  type LocalFontEntry,
} from "../../lib/tauri-api";
import { assertAssShape, insertFontsSection } from "./ass-font-section";
import {
  assertSafeOutputFilename,
  assertSafeOutputPath,
  decomposeInputPath,
} from "../../lib/path-validation";
import { sanitizeError } from "../../lib/dedup-helpers";
import { stripUnicodeControls } from "../../lib/unicode-controls";

// ── Types ─────────────────────────────────────────────────

/** Where a resolved font came from. Shown as a badge in the main font list.
 *  Named `FontProvenance` (not `FontSource`) to avoid colliding with
 *  `FontSource` in FontSourceModal, which is a struct describing a
 *  user-picked source (folder or file set).
 *
 *  - "local"  — this run's font sources (in-memory map or session DB).
 *  - "cache"  — persistent gui_font_cache (#5); fonts indexed in a
 *               previous launch or via the CLI's refresh-fonts.
 *  - "system" — OS-installed system fonts via font-kit. */
export type FontProvenance = "local" | "cache" | "system";

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
const USER_FONT_KEY_SEP = "\u001F";
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

export interface EmbedFontsResult {
  content: string;
  embeddedCount: number;
  warnings: string[];
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
  // Full-face/PostScript aliases are fallback keys. Insert them after
  // every exact family key and never overwrite, so an alias cannot
  // steal a real family/style match from another face.
  for (const face of faces) {
    const familyLookupKeys = new Set(
      face.families.map((family) => family.normalize("NFC").toLowerCase())
    );
    const faceNames = face.faceNames ?? face.face_names ?? [];
    for (const faceName of faceNames) {
      if (familyLookupKeys.has(faceName.normalize("NFC").toLowerCase())) continue;
      for (const bold of [false, true]) {
        for (const italic of [false, true]) {
          const key = userFontKey(faceName, bold, italic);
          if (!map.has(key)) map.set(key, face);
        }
      }
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
 * @param systemFontCache - Optional batch-shared cache mapping a font key to
 *                          a `findSystemFont` resolution. Without this, every
 *                          file in an N-file batch repeats the same
 *                          `findSystemFont` IPC for the same missing fonts.
 * @param useRustUserFonts - Production path: ask Rust's session-local source
 *                           index for a match before falling back to system.
 * @param cacheLookupCache - Optional batch-shared cache mapping a font key
 *                           to its persistent-cache lookup outcome (hit OR
 *                           miss stored as null). Without this, every file
 *                           in an N-file batch repeats the same
 *                           `lookupFontFamily` IPC for the same fonts —
 *                           N×M IPC calls even when answers are stable.
 *                           Symmetric with `systemFontCache`. Note: `get()`
 *                           returns `undefined` for "not yet looked up";
 *                           `null` is stored for "known cache miss" so the
 *                           three states stay distinguishable (untried /
 *                           hit / known-miss).
 */
export async function analyzeFonts(
  assContent: string,
  userFontMap?: Map<string, LocalFontEntry> | null,
  systemFontCache?: Map<string, SystemFontResolution>,
  useRustUserFonts = false,
  cacheLookupCache?: Map<string, { path: string; index: number } | null>
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

  // The `source` field on each FontInfo (`local` / `cache` / `system`)
  // reflects the resolution tier captured at ANALYSIS time. Mid-session
  // changes to the persistent cache (e.g., a CLI `refresh-fonts` run
  // happening while the GUI is open) will not retroactively update the
  // badge in the detection grid — only the next analyze pass refreshes
  // it. The actual filePath stays valid because the file still exists
  // either way; only the UI badge would be technically stale, not the
  // embed result.
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

    // Persistent font cache (#5) — sits between the session-DB user
    // sources and the OS system-font fallback. Mirrors the CLI's
    // resolve_embed_font tier order. Returns null when the family
    // isn't cached OR when the cache is unavailable (init failure /
    // schema mismatch); both fall through to system lookup. Dedup
    // via cacheLookupCache so an N-file batch doesn't repeat the
    // same per-font IPC N times (mirrors systemFontCache below).
    let cacheResult = cacheLookupCache?.get(key);
    if (cacheResult === undefined) {
      try {
        cacheResult = await lookupFontFamily(usage.key.family, usage.key.bold, usage.key.italic);
      } catch (error) {
        if (isDev)
          console.debug(`[ssaHdrify] persistent cache lookup failed; falling through`, error);
        cacheResult = null;
      }
      cacheLookupCache?.set(key, cacheResult);
    }
    if (cacheResult) {
      if (isDev) console.debug(`[ssaHdrify] '${usage.key.family}' → CACHE ${cacheResult.path}`);
      infos.push({
        ...base,
        filePath: cacheResult.path,
        fontIndex: cacheResult.index,
        error: null,
        source: "cache",
      });
      continue;
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
      // sanitizeError so a BiDi/zero-width char carried by a Rust IPC
      // error message can't surface in the UI through
      // `SystemFontResolution.error` (which the detection grid renders).
      const reason = sanitizeError(e);
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
// FNV-1a-INSPIRED, not FNV-1a: standard FNV-1a runs on a byte stream,
// typically the UTF-8 encoding of the input. This
// variant iterates over Unicode codepoints and mixes each one as three
// 8-bit chunks (low / mid / high) — a stable, deterministic hash with
// the same shape but distinct outputs from a textbook FNV-1a on the
// UTF-8 bytes of the same string. The function's only contract is
// "stable, distinct outputs for distinct inputs"; the name is
// `familyStableHash` to avoid promising a wire-compatible FNV-1a.
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
export function buildFontFileName(key: FontKey): string {
  let name = key.family
    .toLowerCase()
    .replace(/[^a-z0-9_-]/g, "_") // strip everything except safe chars
    .replace(/_+/g, "_") // collapse consecutive underscores
    .replace(/^_|_$/g, ""); // trim leading/trailing underscores
  // When the ASCII-only strip empties the name — common for pure-CJK family
  // names — append a stable hash of the original so distinct CJK fonts don't
  // collide on the same `font.ttf` filename inside the [Fonts] section.
  if (!name) name = `font_${familyStableHash(key.family)}`;
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
  // Decompose via the shared helper. Validates absolute, accepts drive-
  // root files (`C:\foo.ass`), rejects drive-relative (`C:foo.ass`).
  const parts = decomposeInputPath(inputPath);
  const { dir, normalized, usedBackslash } = parts;
  let { baseName } = parts;
  // Sibling parity with deriveShiftedPath + resolveOutputPath's `.hdr`
  // strip: strip a prior `.embedded` infix so re-embedding
  // `EP01.embedded.ass` yields `EP01.embedded.ass` (idempotent) rather
  // than the cumulative `EP01.embedded.embedded.ass`. Without this, a
  // user re-running embed on already-embedded output would see the
  // cumulative form on disk — surprising once noticed.
  if (baseName.toLowerCase().endsWith(".embedded")) {
    baseName = baseName.slice(0, -".embedded".length);
  }
  // Post-strip baseName empty / whitespace-or-dot-only guard mirrors
  // deriveShiftedPath : a POSIX dotfile-shape like
  // `.embedded.ass` whose stem is just `.` would otherwise resolve
  // to `.embedded.ass` unchanged on every re-run.
  if (!baseName || !baseName.replace(/^\.+/, "").trim()) {
    throw new Error("Input filename has no valid stem after stripping .embedded infix");
  }
  // Output is always .ass — the embed step rebuilds an ASS-format file
  // regardless of whether the input was .ass or .ssa.
  const outputName = `${baseName}.embedded.ass`;
  // Apply the shared safety checks (reserved names, traversal,
  // MAX_PATH, self-overwrite). Same helpers as HDR / Shift resolvers.
  assertSafeOutputFilename(outputName);
  const outputPath = `${dir}/${outputName}`;
  assertSafeOutputPath(outputPath, normalized);
  return usedBackslash ? outputPath.replace(/\//g, "\\") : outputPath;
}

// Source of truth: `app_lib::fonts::MAX_SUBSET_CODEPOINTS` in
// `src-tauri/src/fonts.rs` (currently 200,000). The TS dedup checks
// the merged-union size BEFORE crossing the `subsetFont` IPC so we
// can decide between (a) one IPC call with the union or (b) N IPC
// calls per alias. A merged union exceeding this cap means a crafted
// ASS has packed several near-per-variant-cap aliases onto one face;
// honest workflows produce unions in the low tens of thousands even
// on full-CJK subtitle batches.
//
// Three values must stay in lockstep —
//   1. `app_lib::fonts::MAX_SUBSET_CODEPOINTS` (the IPC cap)
//   2. `MAX_SUBSET_CODEPOINTS_FOR_DEDUP` in CLI `bin/cli/main.rs`
//   3. this constant (the GUI dedup cap)
// The Rust side has a `dedup_cap_matches_ipc_cap` test pinning
// values 1 and 2. Hoisted to module scope + exported so the TS test
// in `font-embedder.test.ts` (`MAX_SUBSET_CODEPOINTS_FOR_DEDUP value
// pin`) can assert the literal value — a unilateral bump on this
// side flips that test red and forces the editor to confirm they
// updated Rust too. Drift between (1) and (3) would cause the GUI
// to attempt unions up to a stale 200k while subset_font rejected
// anything over the new IPC cap — the dedup-fallback contract from
// `b8aa3fd` breaks.
export const MAX_SUBSET_CODEPOINTS_FOR_DEDUP = 200_000;

/**
 * Embed selected fonts into an ASS file.
 *
 * @param assContent - Original ASS file content
 * @param selectedFonts - Font infos to embed (must have valid filePaths)
 * @param fontUsages - Full font usage data (for codepoint sets)
 * @param onProgress - Optional progress callback
 * @param isCancelled - Optional cancellation probe; returning true mid-loop
 *                      stops the embed and resolves to `null`. Polled per
 *                      font so a multi-font batch responds promptly to user
 *                      cancellation.
 * @param t - Optional i18n translator. When omitted, error notes use raw
 *            English keys (test paths); when present, notes are localized
 *            via the same `useI18n` instance the calling tab uses.
 * @returns Modified ASS content with [Fonts] section and non-fatal
 *          warnings, or `null` when isCancelled returned true mid-embed.
 */
export async function embedFonts(
  assContent: string,
  selectedFonts: FontInfo[],
  fontUsages: FontUsage[],
  onProgress?: (progress: EmbedProgress) => void,
  isCancelled?: () => boolean,
  t?: (key: string, ...args: (string | number)[]) => string
): Promise<EmbedFontsResult | null> {
  const fontEntries: string[] = [];
  const warnings: string[] = [];

  // Index fontUsages by lookup key once (O(N)) instead of linear-scanning
  // for each selected font (O(N²)). For a 40-font subtitle the scan cost
  // is negligible, but the index also makes the lookup intent explicit
  // and matches how analyzeFonts builds its own keyed maps.
  const usageByKey = new Map<string, FontUsage>();
  for (const u of fontUsages) {
    usageByKey.set(userFontKey(u.key.family, u.key.bold, u.key.italic), u);
  }

  // Group FontInfo entries by the underlying resolved face — the
  // `(filePath, fontIndex, bold, italic)` tuple. Family aliases that
  // resolve to the same face (e.g., the English `Microsoft YaHei` and
  // Chinese `微软雅黑` both pointing at `msyh.ttc` face 0) otherwise
  // produce duplicate subset operations with byte-identical payloads
  // embedded under different `fontname:` filenames — measured ~34%
  // of bundle size on a 3-style CJK ASS against `msyh.ttc`. The
  // preserved name-table records in `fonts.rs::subset_with_index`
  // (every name ID, every language) let libass match every original
  // family alias to the single deduped entry's internal name table.
  //
  // First-occurrence wins for the FontInfo used as the group
  // template (filename, font_name label). The codepoint set unions
  // across every alias that resolved to this face.
  //
  // Separator MUST be U+001F (Unit Separator) — same convention as
  // userFontKey at line 78 and user_font_key in fonts.rs. An empty
  // separator collides distinct face tuples whose path digits and
  // fontIndex digits adjoin: ("foo", 11, 0, 0) and ("foo1", 1, 0, 0)
  // would both fold to "foo1100" and silently drop one face from
  // the output. Written in escape form (not as the raw byte) so a
  // grep / cat / web diff cannot misread it as the empty string and
  // file the same false-positive that R1 caught.
  const FACE_DEDUP_SEP = "\u001F";
  const faceDedupKey = (info: FontInfo): string =>
    `${info.filePath}${FACE_DEDUP_SEP}${info.fontIndex}${FACE_DEDUP_SEP}${info.key.bold ? "1" : "0"}${FACE_DEDUP_SEP}${info.key.italic ? "1" : "0"}`;

  // Per-alias entries are preserved so the main-pass fallback can
  // subset each alias separately when the merged-union codepoint
  // count would exceed the downstream `subset_font` cap (see
  // `MAX_SUBSET_CODEPOINTS_FOR_DEDUP` below). The happy path still
  // takes the dedup win by computing the union lazily and calling
  // subsetFont once; the fallback only fires for the contrived
  // cap-busting case (4+ aliases each near `MAX_CODEPOINTS_PER_VARIANT`
  // all resolving to the same face).
  interface FaceAlias {
    info: FontInfo;
    codepoints: Set<number>;
  }
  interface FaceGroup {
    template: FontInfo;
    aliases: FaceAlias[];
  }
  const faceGroups = new Map<string, FaceGroup>();

  // Pre-pass: build dedup groups + emit no-usage diagnostics. The
  // no-usage warning fires when analyzeFonts produced a FontInfo but
  // its usage record is absent from fontUsages — analysis-time
  // disagreement, not embed-time dedup. Progress numbers track the
  // pre-dedup iteration so the user can correlate warnings with the
  // FontInfo list order.
  const preTotal = selectedFonts.length;
  for (let preIdx = 0; preIdx < selectedFonts.length; preIdx++) {
    if (isCancelled?.()) return null;
    const info = selectedFonts[preIdx]!;
    if (!info.filePath) continue;

    const label = fontKeyLabel(info.key);
    const usage = usageByKey.get(userFontKey(info.key.family, info.key.bold, info.key.italic));
    if (!usage) {
      // Selected FontInfo has no matching FontUsage — means analyzeFonts and
      // the current fontUsages array disagree, which should be impossible if
      // both came from the same ASS parse. Surface the drift through
      // onProgress so the user sees in the log panel that a selected font
      // silently won't embed — the previous `console.warn`-only path left
      // the user with no feedback. Reuses the existing `msg_font_skipped`
      // i18n key for consistency with the subsetting-failure path below.
      // (Reuses `label` from the outer scope.)
      // Pattern 1 sibling completion: `label` is `fontKeyLabel(info.key)`
      // where info.key.family flows from V8-parsed ASS `\fn` (P1b
      // attacker-influenced). The subset-failure site below already
      // scrubs; this no-usage-entry sibling stayed raw. Cheap
      // stripUnicodeControls wrap mirrors the subset-failure
      // sanitizeError(...) posture for the same Pattern 1
      // single-source-completion reason.
      console.warn(`[ssaHdrify] embedFonts: no usage entry for ${stripUnicodeControls(label)}`);
      // Pattern 1 sibling parity: wrap info.key.family in
      // stripUnicodeControls for the t?.() args + English fallback,
      // mirroring the subset-failure sibling below. Upstream
      // `sanitizeFamily` already strips controls so the surface is
      // closed today; the wrap keeps every sibling output consistent
      // if upstream sanitization ever loosens (defense-in-depth).
      const familyDisplay = stripUnicodeControls(info.key.family);
      const warning =
        t?.("msg_font_skipped", familyDisplay, "no usage entry") ??
        `Skipped ${familyDisplay}: no usage entry`;
      warnings.push(warning);
      onProgress?.({
        stage: warning,
        current: preIdx + 1,
        total: preTotal,
      });
      continue;
    }

    // Append this alias's per-info codepoint set to the resolved-face
    // group. The first FontInfo seen for a given face becomes the
    // template (its family drives `buildFontFileName` in the
    // dedup-happy path); subsequent aliases contribute their own
    // FaceAlias entry. Keeping per-alias entries (instead of eagerly
    // unioning into one Set) lets the main-pass fallback subset each
    // alias separately when the merged union would exceed
    // `MAX_SUBSET_CODEPOINTS_FOR_DEDUP`.
    const fKey = faceDedupKey(info);
    const aliasEntry: FaceAlias = { info, codepoints: new Set(usage.codepoints) };
    const existing = faceGroups.get(fKey);
    if (existing) {
      existing.aliases.push(aliasEntry);
    } else {
      faceGroups.set(fKey, { template: info, aliases: [aliasEntry] });
    }
  }

  // Main pass: subset + embed once per unique resolved face — UNLESS
  // the merged-union codepoint count would exceed the downstream
  // subset cap, in which case fall back to per-alias subsetting for
  // that face only (pre-2a-i shape, restricted to the cap-busting
  // edge case so honest inputs keep the dedup win).
  //
  // Progress numbers reflect the deduped work in the happy path,
  // so a 3-alias / 1-face input shows `1/1` instead of `1/3` —
  // matches what the user actually sees in the output [Fonts]
  // section. In the fallback path, progress is per-alias for that
  // group (the user sees N entries appear), aligned with the
  // pre-2a-i behavior they had before.
  const groups = Array.from(faceGroups.values());
  const groupTotal = groups.length;
  for (let i = 0; i < groups.length; i++) {
    if (isCancelled?.()) return null;
    const group = groups[i]!;
    const { template, aliases } = group;
    if (!template.filePath) continue; // structural unreachable — pre-pass filtered nulls

    // Compute the merged union lazily, then decide between the
    // dedup path and the per-alias fallback. The union is the
    // happy-path payload; if it overflows the cap, we throw it
    // away and subset each alias separately.
    //
    // Pattern 2 — bound the iteration cost: early-exit the merge loop
    // on the iteration where the union FIRST exceeds the cap. The size
    // check fires AFTER add(), so the Set holds
    // `MAX_SUBSET_CODEPOINTS_FOR_DEDUP + 1` (200,001) entries at the
    // break point. Upstream caps (`MAX_TOTAL_CODEPOINTS` 1M,
    // `MAX_FONT_VARIANTS` 500) bound the iteration TRANSITIVELY, but
    // locally the loop reads as unbounded; this caps worst-case work
    // at 200,001 Set inserts instead of the transitive 1M-codepoint
    // walk.
    const mergedCodepoints = new Set<number>();
    let capExceeded = false;
    outer: for (const alias of aliases) {
      for (const cp of alias.codepoints) {
        mergedCodepoints.add(cp);
        if (mergedCodepoints.size > MAX_SUBSET_CODEPOINTS_FOR_DEDUP) {
          capExceeded = true;
          break outer;
        }
      }
    }

    if (capExceeded) {
      // Cap-busting fallback: subset each alias independently. Each
      // alias's codepoints is bounded by `MAX_CODEPOINTS_PER_VARIANT`
      // upstream (font-collector), which is strictly less than the
      // subset cap, so individual subset calls always pass. Output
      // for this face reverts to pre-2a-i shape (one [Fonts] entry
      // per alias, byte-identical payloads under different
      // filenames); the dedup byte-reduction win is given up only
      // for this specific group. The face's family-name records
      // are still preserved at the Rust subset layer, so libass's
      // per-glyph fallback can traverse the N entries to find any
      // requested glyph.
      //
      // Overlap trade-off, not a bug: real-world cap-busts usually
      // involve aliases whose codepoint sets
      // overlap (e.g., two CJK aliases both covering the same
      // ideograph range). The fallback subsets each alias with
      // its OWN codepoints, so overlapping codepoints get embedded
      // multiple times under different filenames — the dedup byte-
      // reduction win is given up MORE than necessary for overlapping
      // aliases. A pair-merge variant (combine aliases whose union
      // stays under the cap before falling back to per-alias) would
      // recover some of that win, but adds an O(N²) merge step for a
      // deliberately-rare cap-busting case. Rendering remains
      // correct; the cost is purely [Fonts] section size.
      for (const alias of aliases) {
        if (isCancelled?.()) return null;
        if (!alias.info.filePath) continue;
        const aliasFontName = buildFontFileName(alias.info.key);
        const aliasLabel = fontKeyLabel(alias.info.key);
        onProgress?.({
          stage: t?.("msg_subsetting", aliasLabel) ?? `Subsetting ${aliasLabel}…`,
          current: i + 1,
          total: groupTotal,
        });
        let aliasSubsetData: Uint8Array;
        try {
          aliasSubsetData = await subsetFont(
            alias.info.filePath,
            alias.info.fontIndex,
            Array.from(alias.codepoints)
          );
          if (isCancelled?.()) return null;
        } catch (subsetErr) {
          const safeErr = sanitizeError(subsetErr);
          console.warn(
            `Font subsetting failed for ${stripUnicodeControls(alias.info.key.family)}, skipping: ${safeErr}`
          );
          const familyDisplay = stripUnicodeControls(alias.info.key.family);
          const warning =
            t?.("msg_font_skipped", familyDisplay, safeErr) ??
            `Skipped ${familyDisplay}: ${safeErr}`;
          warnings.push(warning);
          onProgress?.({
            stage: warning,
            current: i + 1,
            total: groupTotal,
          });
          continue;
        }
        fontEntries.push(buildFontEntry(aliasFontName, aliasSubsetData));
      }
      continue; // Group handled; advance to next face.
    }

    const fontName = buildFontFileName(template.key);
    const label = fontKeyLabel(template.key);
    onProgress?.({
      stage: t?.("msg_subsetting", label) ?? `Subsetting ${label}…`,
      current: i + 1,
      total: groupTotal,
    });

    let subsetData: Uint8Array;
    try {
      subsetData = await subsetFont(
        template.filePath,
        template.fontIndex,
        Array.from(mergedCodepoints)
      );
      if (isCancelled?.()) return null;
    } catch (subsetErr) {
      // sanitizeError (Pattern 1 callsite census). The Rust subset_font
      // error string can interpolate font-file paths (P1b — fan-sub
      // font packs are attacker-influenced content). The error flows
      // into progress.stage which the log panel renders directly;
      // without scrubbing, a font with a BiDi-bearing name or path can
      // visually reverse adjacent text in the log line. sanitizeError
      // combines message-extraction (drops the "Error: " prefix
      // String(e) prepends) with the same scrub, keeping every catch
      // arm uniform.
      const safeErr = sanitizeError(subsetErr);
      // Dev-console warn must scrub too. Without scrubbing, this site
      // interpolates raw `subsetErr` on the line immediately above the
      // sanitized onProgress dispatch — re-introducing exactly the
      // BiDi disclosure surface the comment block motivating the scrub
      // argues against. WebView2's dev-tools surface is opt-in in
      // production, so blast radius is small, but Pattern 1
      // single-source completion requires every sibling output to use
      // the same helper.
      // Pattern 1 defense-in-depth: `usage.key.family` is
      // upstream-sanitized at font-collector.ts::sanitizeFamily so the
      // BiDi/control surface is closed today. But the comment block
      // above motivates "every sibling output uses the same helper" —
      // that discipline argument applies here too. Cheap defensive
      // wrap keeps the pattern consistent if upstream sanitizeFamily
      // ever loosens.
      console.warn(
        `Font subsetting failed for ${stripUnicodeControls(template.key.family)}, skipping: ${safeErr}`
      );
      // Pattern 1 sibling parity: wrap template.key.family for t?.()
      // args + English fallback; same sibling-parity reasoning as the
      // no-usage-entry path above.
      const familyDisplay = stripUnicodeControls(template.key.family);
      const warning =
        t?.("msg_font_skipped", familyDisplay, safeErr) ?? `Skipped ${familyDisplay}: ${safeErr}`;
      warnings.push(warning);
      onProgress?.({
        stage: warning,
        current: i + 1,
        total: groupTotal,
      });
      continue; // Skip this face, don't fall back to unguarded read
    }

    // Build the [Fonts] entry
    fontEntries.push(buildFontEntry(fontName, subsetData));
  }

  if (isCancelled?.()) {
    return null;
  }

  // the zero-fontEntries early-return used to
  // skip the insertFontsSection [Script Info] guard.
  // A malformed ASS referencing zero fonts would succeed as a no-op
  // and downstream persisted the still-malformed file untouched —
  // the embed reported success against input that the GUI would
  // refuse to re-open. Run the [Script Info] check upfront via
  // assertAssShape so the contract is "valid ASS in, valid ASS out
  // (or throw)" regardless of font count.
  assertAssShape(assContent);
  if (fontEntries.length === 0) {
    // zero-font early-return returns
    // `assContent` verbatim — no [Fonts] insertion means no
    // separator normalization either. The non-zero path below
    // routes through `insertFontsSection` which strips
    // U+2028 / U+2029 line separators (the rewrite path needs
    // ASCII newlines for the column-0 [Fonts] header regex to
    // anchor). When there are no fonts to insert, the input is
    // returned untouched — Unicode line separators in the original
    // ASS survive intact. Documented so a future
    // "normalize-on-every-output" refactor doesn't accidentally
    // strip them here, which would inflate diff noise on files the
    // user never asked us to transform.
    return { content: assContent, embeddedCount: 0, warnings };
  }

  // Build [Fonts] section (no leading \n — insertFontsSection handles the separator)
  const fontsSection = `[Fonts]\n${fontEntries.join("\n\n")}\n`;

  // Insert [Fonts] section into ASS file
  return {
    content: insertFontsSection(assContent, fontsSection),
    embeddedCount: fontEntries.length,
    warnings,
  };
}

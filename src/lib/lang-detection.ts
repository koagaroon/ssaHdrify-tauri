/**
 * Language-tag detection for subtitle filenames.
 *
 * Recognizes common language codes that fan-sub naming conventions
 * append before the extension (e.g., `EP01.zh.ass`, `EP01.sc.ass`).
 * Used by the HDR Convert {lang} token (`output-naming.ts`) and the
 * Batch Rename output path derivation (`pairing-engine.ts`); shared
 * here so the recognized set stays consistent across both.
 *
 * Limited to the subset fan-sub workflows actually use; widening
 * later is cheap.
 */

export const LANG_TAGS: ReadonlySet<string> = new Set([
  "zh",
  "zh-cn",
  "zh-tw",
  "en",
  "ja",
  "jp",
  "ko",
  "fr",
  "de",
  "es",
  "it",
  "ru",
  "pt",
  "chs",
  "cht",
  "jpn",
  "eng",
  "kor",
  "sc",
  "tc",
]);

export function canonicalLanguageTag(language: string): string {
  switch (language.toLowerCase()) {
    case "chs":
    case "sc":
    case "zh":
    case "zh-cn":
      return "sc";
    case "cht":
    case "tc":
    case "zh-tw":
      return "tc";
    case "ja":
    case "jpn":
    case "jp":
      return "jp";
    case "eng":
      return "en";
    case "ko":
    case "kor":
      return "ko";
    default:
      return language.toLowerCase();
  }
}

/**
 * Extract a known language tag from a basename's last dotted segment.
 * Returns "" when no recognized tag is present. Case-folded for
 * matching; the returned value is lowercase. Operates on the base
 * filename WITHOUT its extension — caller is responsible for stripping
 * the file extension first if relevant.
 */
export function extractLangFromBaseName(baseName: string): string {
  const dotIdx = baseName.lastIndexOf(".");
  // Reject -1 (no dot at all) AND 0 (a leading-dot name like ".zh" — that's
  // a hidden file with no real basename, not a "zh-tagged" subtitle).
  if (dotIdx <= 0) return "";
  const candidate = baseName.slice(dotIdx + 1).toLowerCase();
  return LANG_TAGS.has(candidate) ? candidate : "";
}

export function subtitleLanguageFromName(name: string): string {
  const dot = name.lastIndexOf(".");
  const baseName = dot > 0 ? name.slice(0, dot) : name;
  return canonicalLanguageTag(extractLangFromBaseName(baseName));
}

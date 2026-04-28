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

/**
 * Extract a known language tag from a basename's last dotted segment.
 * Returns "" when no recognized tag is present. Case-folded for
 * matching; the returned value is lowercase. Operates on the base
 * filename WITHOUT its extension — caller is responsible for stripping
 * the file extension first if relevant.
 */
export function extractLangFromBaseName(baseName: string): string {
  const dotIdx = baseName.lastIndexOf(".");
  if (dotIdx <= 0) return "";
  const candidate = baseName.slice(dotIdx + 1).toLowerCase();
  return LANG_TAGS.has(candidate) ? candidate : "";
}

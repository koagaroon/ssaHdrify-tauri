/**
 * Shared rejection set for Trojan-Source and zero-width attacks.
 *
 * Mirrors the Rust-side `validate_font_family` / `validate_ipc_path`
 * codepoint enumeration in `src-tauri/src/util.rs`. Single source of
 * truth so TS-side validators don't drift apart from each other or
 * from the Rust side as new attack patterns are added.
 *
 * Coverage:
 * - U+061C — Arabic Letter Mark (bidi format Cf)
 * - U+180E — Mongolian Vowel Separator (legacy invisible)
 * - U+200B..U+200D — ZWSP / ZWNJ / ZWJ (zero-width)
 * - U+200E / U+200F — LRM / RLM (bidi marks)
 * - U+202A..U+202E — LRE / RLE / PDF / LRO / RLO (bidi embedding +
 *                    override; U+202E is the well-known
 *                    filename-display-reversal vector,
 *                    CVE-2021-42574 Trojan-Source class)
 * - U+2028 / U+2029 — line / paragraph separators (Unicode line
 *                     breaks that smuggle past `\r?\n` splitters)
 * - U+2060 — WORD JOINER (invisible)
 * - U+2066..U+2069 — LRI / RLI / FSI / PDI (bidi isolates)
 * - U+FEFF — ZWNBSP / BOM-in-the-middle
 *
 * Round 6 Wave 6.2 brought U+2060 and U+180E into this central set
 * for symmetry with the Rust-side rejections — the Round 5 note
 * "Rust catches them upstream" only held for paths that round-trip
 * through Rust, leaving sanitizeFamily / ass-uuencode safeName (both
 * pure TS) blind to the same codepoints the Rust validators
 * rejected.
 */
const BIDI_AND_ZERO_WIDTH_PATTERN =
  "\\u061C\\u180E\\u200B-\\u200F\\u202A-\\u202E\\u2028\\u2029\\u2060\\u2066-\\u2069\\uFEFF";

/** Inline character class fragment for splicing into composite regexes. */
export const BIDI_AND_ZERO_WIDTH_CHARS = BIDI_AND_ZERO_WIDTH_PATTERN;

/** Standalone matcher; use `.test(s)` for reject-on-match validation. */
export const BIDI_AND_ZERO_WIDTH_RE = new RegExp(`[${BIDI_AND_ZERO_WIDTH_PATTERN}]`, "u");

/** Global matcher; use with `.replace(GLOBAL_RE, "")` for scrubbing. */
export const BIDI_AND_ZERO_WIDTH_GLOBAL_RE = new RegExp(`[${BIDI_AND_ZERO_WIDTH_PATTERN}]`, "gu");

/** True if `s` contains any character in the rejection set. */
export function hasUnicodeControls(s: string): boolean {
  return BIDI_AND_ZERO_WIDTH_RE.test(s);
}

/** Strip all characters in the rejection set; safe for display / log paths. */
export function stripUnicodeControls(s: string): string {
  return s.replace(BIDI_AND_ZERO_WIDTH_GLOBAL_RE, "");
}

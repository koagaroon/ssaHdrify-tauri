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
import { BIDI_AND_ZERO_WIDTH_CHARS } from "../../lib/unicode-controls";

// Hoisted to module scope (A-R5-FECHAIN-12) so the `\p{L}` Unicode
// property regex compiles once instead of per override block. Used by
// the `\r<style>` reset detector inside `walkText`.
const R_RESET_RE = /\\r(?=\\|$|[\p{L}_])/u;

// Lazy dynamic import — only triggers when ensureLoaded() is first called.
// Previously this ran at module load time, which blocked startup after the
// CSS visibility refactor made all tabs mount immediately.
export type AssParseFunction = (text: string) => ParsedASS;

let parseFn: AssParseFunction | null = null;
let assCompilerReady: Promise<void> | null = null;

/**
 * Defense-in-depth caps against crafted ASS input. See `collectFonts` for how
 * they interact — per-variant + total are both needed; either alone can be
 * blown past by the other dimension.
 *
 * MAX_CODEPOINTS_PER_VARIANT (65536) is a defensive cap, not the Basic
 * Multilingual Plane boundary (which caps at U+FFFF). Real fonts carry well
 * under this; the cap only fires against crafted ASS enumerating tens of
 * thousands of distinct characters for one font.
 */
const MAX_FONT_VARIANTS = 500;
const MAX_CODEPOINTS_PER_VARIANT = 65536;
const MAX_TOTAL_CODEPOINTS = 1_000_000;

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

/** Format a FontKey as a human-readable label (e.g., "Arial Bold Italic"). */
export function fontKeyLabel(key: FontKey): string {
  let label = key.family;
  if (key.bold) label += " Bold";
  if (key.italic) label += " Italic";
  return label;
}

/**
 * Strip the ASS `@` vertical-writing prefix from a family name.
 *
 * `@FamilyName` in a Style or `\fn` override tag tells the renderer to
 * rotate glyphs 90° for vertical typesetting — the underlying font file is
 * identical to the non-prefixed form. For font *identification* (matching,
 * subsetting, embedding) we must treat both as the same font, so this strip
 * is applied consistently wherever the collector captures a family name.
 */
function normalizeFamily(raw: string): string {
  const trimmed = raw.trim();
  return trimmed.startsWith("@") ? trimmed.slice(1) : trimmed;
}

/** Strip control characters and cap length — applied to every family name
 *  captured from a subtitle file before it flows into matching or output.
 *
 *  Round 10 A-R10-010 note on naming: despite the `sanitize` prefix,
 *  this helper performs NORMALIZATION (strip + truncate) — it never
 *  rejects, only transforms. Compare to Rust-side `validate_font_family`
 *  which throws on the same codepoint set. The two roles are
 *  intentionally different:
 *
 *  - TS `sanitizeFamily` runs at the TS engine layer (parse a subtitle
 *    → collect font usages → present in detection grid). The family
 *    name is the user's content; we want to display it (possibly
 *    truncated and scrubbed) rather than refuse the whole subtitle.
 *
 *  - Rust `validate_font_family` runs at IPC entry and SQL-bind
 *    boundaries. Hostile inputs reaching the trust set / persistence
 *    layer SHOULD be rejected, not silently normalized into a
 *    different family name (which could shadow a legitimate font row).
 *
 *  A future "fix the asymmetry" refactor that tightens TS to reject
 *  would break legitimate inputs (a subtitle with a BiDi-bearing font
 *  name would refuse to render); a refactor that loosens Rust to
 *  normalize would smuggle hostile content past the trust gate. The
 *  asymmetry is load-bearing — keep both.
 *
 *  Range covers C0 (0x00-0x1F), DEL (0x7F), C1 (0x80-0x9F), the Unicode
 *  line/paragraph separators (U+2028 / U+2029), AND the full
 *  BiDi / zero-width control set from `unicode-controls.ts` (Round 6
 *  Wave 6.2 parity sweep — previously a family name carrying U+202E
 *  could flow through `sanitizeFamily` into detection-grid labels, log
 *  lines, and chain progress text where the visual-reversal attack
 *  re-surfaced after `safeName` had already scrubbed it on the [Fonts]
 *  header path). Full parity with `ass-uuencode.ts::safeName` on the
 *  shared codepoints — see `sanitization.test.ts` for the pin.
 *
 *  Exported for the cross-helper symmetry pin test (Round 2 N-R2-17):
 *  the parity claim between this helper and `ass-uuencode::buildFontEntry`'s
 *  inline `safeName` is enforced by a test that exercises both sides on
 *  the same input range. */
export function sanitizeFamily(raw: string): string {
  // Control characters reach the regex via the dynamically-built
  // `new RegExp(...)` form rather than a regex literal — eslint's
  // `no-control-regex` only inspects literals, so no inline disable
  // directive is needed. Behavior is identical to a literal regex
  // (codepoint classes are evaluated at the same runtime stage).
  return raw
    .replace(new RegExp(`[\\x00-\\x1f\\x7f-\\x9f${BIDI_AND_ZERO_WIDTH_CHARS}]`, "gu"), "")
    .slice(0, 128);
}

/**
 * Ensure ass-compiler is loaded. Call before using collector functions.
 */
export async function ensureLoaded(): Promise<void> {
  if (!assCompilerReady) {
    assCompilerReady = import("ass-compiler")
      .then((m) => {
        parseFn = m.parse;
      })
      .catch((e) => {
        assCompilerReady = null; // allow retry on next call
        throw e;
      });
  }
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
  return collectFontsWithParser(assContent, parseFn);
}

export function collectFontsWithParser(assContent: string, parser: AssParseFunction): FontUsage[] {
  // Parse ASS file
  const parsed = parser(assContent);
  if (!parsed) {
    throw new Error("Failed to parse ASS file");
  }

  // Build style → font map from [V4+ Styles]
  const styleMap = new Map<string, { family: string; bold: boolean; italic: boolean }>();

  if (parsed.styles?.style) {
    for (const style of parsed.styles.style) {
      // Drop the ASS `@` vertical-writing prefix (collapses vertical and
      // horizontal uses into one entry), then strip control chars and cap length.
      const family = sanitizeFamily(normalizeFamily(style.Fontname || "Arial"));
      styleMap.set(style.Name, {
        family: family || "Arial",
        bold: parseInt(style.Bold || "0", 10) !== 0,
        italic: parseInt(style.Italic || "0", 10) !== 0,
      });
    }
  }

  // Accumulate: fontKeyString → { key, codepoints }
  const usageMap = new Map<string, FontUsage>();
  let totalCodepoints = 0;

  function recordChars(key: FontKey, text: string) {
    const keyStr = fontKeyToString(key);
    let usage = usageMap.get(keyStr);
    if (!usage) {
      usage = { key: { ...key }, codepoints: new Set() };
      usageMap.set(keyStr, usage);
      if (usageMap.size > MAX_FONT_VARIANTS) {
        throw new Error(`Too many font variants: ${usageMap.size} (max ${MAX_FONT_VARIANTS})`);
      }
    }
    for (const char of text) {
      if (usage.codepoints.size >= MAX_CODEPOINTS_PER_VARIANT) {
        break;
      }
      const cp = char.codePointAt(0);
      // Skip control chars (incl. U+007F DEL), ASCII space, and invalid
      // codepoints. Space is dropped here because the Rust subset always
      // pads the full ASCII printable range (0x20–0x7E), so counting it
      // would double-bill what the subset already includes for free.
      // C1 controls (U+0080..U+009F) and other Unicode control characters
      // pass through this filter — Rust's subset_font emits `.notdef`
      // for them harmlessly, so the leak (1 extra codepoint per C1 char
      // in MAX_CODEPOINTS_PER_VARIANT accounting) is bounded and benign
      // (Round 3 A-R3-8).
      if (cp !== undefined && cp > 32 && cp !== 0x7f && cp <= 0x10ffff) {
        const before = usage.codepoints.size;
        usage.codepoints.add(cp);
        if (usage.codepoints.size !== before) {
          totalCodepoints++;
          if (totalCodepoints > MAX_TOTAL_CODEPOINTS) {
            throw new Error(
              `Too many codepoints across fonts: ${totalCodepoints} (max ${MAX_TOTAL_CODEPOINTS})`
            );
          }
        }
      }
    }
  }

  if (parsed.events?.dialogue) {
    for (const dialogue of parsed.events.dialogue) {
      const styleName = dialogue.Style || "Default";
      const baseStyle: FontKey = styleMap.get(styleName) ?? {
        family: "Arial",
        bold: false,
        italic: false,
      };
      const rawText: string = dialogue.Text?.raw ?? "";
      processDialogueText(rawText, baseStyle, styleMap, recordChars);
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
// Per-text length cap. ass-compiler returns the parsed dialogues; an
// upstream parser bug surfacing a giant text in a small input would
// drive O(n²) behavior on brace-light strings (the `text.indexOf("{",
// i)` scans + the per-char step compound). Rust caps total file size at
// 50 MB, so the cumulative budget is bounded — but a single dialogue
// near that ceiling is still pathological. 1 MB per dialogue is
// generous (typical line is 50-500 chars; even concatenated styled
// karaoke songs rarely cross a few KB).
const MAX_DIALOGUE_TEXT_LEN = 1_000_000;

function processDialogueText(
  text: string,
  initialFont: FontKey,
  styleMap: Map<string, FontKey>,
  recordChars: (key: FontKey, text: string) => void
) {
  if (text.length > MAX_DIALOGUE_TEXT_LEN) {
    text = text.slice(0, MAX_DIALOGUE_TEXT_LEN);
  }
  let current = { ...initialFont };
  let isDrawing = false;
  let i = 0;

  while (i < text.length) {
    if (text[i] === "{") {
      // Override block — parse tags until closing }
      const closeIdx = text.indexOf("}", i);
      if (closeIdx === -1) {
        // Malformed override block — treat unmatched '{' as literal text
        // (matches behavior of most ASS renderers like libass/Aegisub).
        // Record all remaining text as rendered glyphs, then stop. We
        // used to `i++; continue;` which is O(n²) on pathological input
        // like `{{{{{…{` — each `{` would indexOf-scan to end of string.
        // Treating the tail as plain text is equivalent under libass's
        // "unmatched-brace means literal" semantics and finishes in O(n).
        if (!isDrawing) {
          const tail = text.slice(i);
          // Strip ASS drawing commands (\N, \n, \h) just like the
          // plain-text branch below (N-R5-FECHAIN-03). Without this,
          // input like `Hello{World\Nfoo` would record literal `\` + `N`
          // codepoints against the per-variant + total caps even
          // though libass treats them as line/space tags, not text.
          const cleanTail = tail.replace(/\\N/g, "").replace(/\\n/g, "").replace(/\\h/g, "");
          if (cleanTail.length > 0) recordChars(current, cleanTail);
        }
        return;
      }

      const block = text.slice(i + 1, closeIdx);
      current = applyOverrideTags(block, current, initialFont, styleMap);
      // Reset drawing on \p0 or \r — checked independently from \p[1-9]
      // so that {\r\p1} correctly resets then re-enables drawing mode.
      // The \r anchor accepts:
      //   - End markers: `\` (next override starts), `}` (block closes),
      //     `$` (end of block) — bare `\r` reset.
      //   - Style-name leading chars: any Unicode letter `\p{L}` plus
      //     underscore. Covers ASCII `\rdefault`, mixed-case `\rJP`,
      //     leading-underscore `\r_Alt`, and CJK `\r字幕` — all valid
      //     style names that the prior `[A-Za-z]` class silently
      //     rejected (Codex 52379e14: subsequent visible text was
      //     attributed to the wrong override font, causing missing
      //     glyphs in embed output).
      // ASS `\p<scale>`: libass parses the full numeric value and
      // treats any positive scale as drawing-on, zero as drawing-off.
      // libass and xy-VSFilter process override tags left-to-right
      // within an override block, so when a block contains MULTIPLE
      // `\p` tags (e.g., `{\p1\p0}` or `{\p0\p1}`), the LAST one
      // determines the resulting drawing state. Using `block.match(...)`
      // (non-global) returns the FIRST match and inverts that semantic
      // — an attacker-controlled `{\p1\p0}` would suppress glyph
      // collection while the renderer correctly keeps drawing-mode
      // OFF, so plain text after the block is collected as drawing
      // commands and missing from the embedded subset (P1b — fan-sub
      // packs are attacker-influenced content sources).
      //
      // `matchAll` + `.at(-1)` gives the LAST occurrence in the block.
      // Round 4 A-R4-07 / Codex 1 follow-up to Round 3 / Codex c94844c3.
      // Regex hoisted to module-level constant `R_RESET_RE` (A-R5-FECHAIN-12)
      // to avoid re-compiling per iteration. The `\}` alternative was
      // also dropped (A-R5-FECHAIN-03): `block` is `text.slice(i + 1,
      // closeIdx)` — the open brace is at i and the close brace is at
      // closeIdx, so `block` never contains `}` and the alternative
      // was dead.
      if (R_RESET_RE.test(block)) {
        isDrawing = false;
      }
      const pTags = [...block.matchAll(/\\p(\d+)/g)];
      const lastP = pTags.at(-1);
      if (lastP) {
        isDrawing = parseInt(lastP[1], 10) > 0;
      }
      i = closeIdx + 1;
    } else {
      // Plain text — find the next override block or end
      const nextBrace = text.indexOf("{", i);
      const plainEnd = nextBrace >= 0 ? nextBrace : text.length;
      const plain = text.slice(i, plainEnd);

      // Skip ASS drawing commands (\N, \n, \h) and line breaks
      const cleanText = plain.replace(/\\N/g, "").replace(/\\n/g, "").replace(/\\h/g, "");

      if (cleanText.length > 0 && !isDrawing) {
        recordChars(current, cleanText);
      }
      i = plainEnd;
    }
  }
}

/**
 * Apply override tags from a single { ... } block to the current font state.
 */
function applyOverrideTags(
  block: string,
  current: FontKey,
  initialFont: FontKey,
  styleMap: Map<string, FontKey>
): FontKey {
  let result = { ...current };

  // \r[StyleName] — ASS spec: bare `\r` resets to the dialogue's initial
  // style; `\r<Name>` resets to the NAMED style (looked up by Style Name
  // in [V4+ Styles]). Earlier code reset to `initialFont` regardless of
  // whether a name was captured, which under-counted codepoints for the
  // named style's font and produced "font not found" rendering when the
  // user's audience didn't have the named style's font installed.
  //
  // Capture group leads on Unicode letter (`\p{L}`) or underscore, then
  // accepts letters / digits / underscore / dash. The prior `[A-Za-z]`
  // start rejected legitimate style names like `_Alt` (Aegisub style
  // imports), `字幕` (CJK-named styles common in fan-sub releases),
  // and `José` — falling through to the bare-`\r` initialFont path
  // even when the named style was defined (Codex 52379e14).
  // `\rdefault` is the canonical "reset to dialogue initial" form and
  // explicitly takes the initialFont path (the literal "default" name
  // in [V4+ Styles] would shadow the dialogue's initial style — by
  // convention `\rdefault` means "the dialogue's initial style", not
  // "the style literally named 'default'").
  // Round 7 Wave 7.5 (N3-R7-5): `matchAll().at(-1)` for left-to-right
  // libass parity. Round 4 A-R4-07 fixed this for `\p<scale>` (the
  // drawing-mode toggle); the same first-match-vs-last-match
  // asymmetry applies to `\r` `\fn` `\b` `\i` when a block contains
  // multiple of the same tag. libass / xy-VSFilter process override
  // tags left-to-right, so a block like `{\fnArial\fnTimes}` ends
  // with the family Times, not Arial. block.match() returns the
  // FIRST match — silently picks Arial here, attacker-influenced
  // ASS (P1b) can use this to make the embedded font set diverge
  // from what libass actually renders. matchAll + .at(-1) gives the
  // LAST occurrence per libass semantics.
  const rMatches = [...block.matchAll(/\\r([\p{L}_][\p{L}\p{N}_-]*)?/gu)];
  const rMatch = rMatches.at(-1);
  if (rMatch) {
    const styleName = rMatch[1];
    if (styleName && styleName.toLowerCase() !== "default" && styleMap.has(styleName)) {
      result = { ...styleMap.get(styleName)! };
    } else {
      result = { ...initialFont };
    }
  }

  // \fn<FontName> — change font family (empty \fn resets to style default).
  // The `@` vertical-writing prefix is a rendering hint, not part of the
  // font identity; strip it so `\fn@Foo` and `\fnFoo` resolve to the same
  // font file and share a FontUsage entry (codepoints merge for subsetting).
  // Cap the capture at 128 chars: `sanitizeFamily` will also slice to 128,
  // but bounding the match keeps allocator cost low against crafted ASS
  // with absurdly long names inside an override block.
  //
  // Exclusion `[^\\}{]` (Round 6 Wave 6.2): `{` is also a stop char so
  // crafted ASS like `\fn{Evil}` doesn't capture `{Evil` as a font name —
  // libass stops the family at any brace boundary and so do we, otherwise
  // the captured "family" carries a literal `{` into matching / output
  // and silently misses the user's intended font.
  // W7.5: matchAll().at(-1) for libass parity — see \r above.
  // Round 8 A-R8-A1-3: also stop at C0 (`\x00-\x1f`) / DEL+C1
  // (`\x7f-\x9f`) / BiDi+zero-width controls. `sanitizeFamily` already
  // strips these AFTER capture, but that lets the capture continue past
  // a hostile `\fn<U+202E>evil` and produce a family-key for matching
  // that diverges from what libass renders (libass keeps the BiDi char
  // in the name → font lookup fails → fallback; our embed strips →
  // matches a real font named `evil` → embed picks a font libass would
  // never render). Stopping the capture at the first control char
  // collapses both paths to "no match → fallback".
  const fnMatches = [
    ...block.matchAll(
      new RegExp(`\\\\fn([^\\\\}{\\x00-\\x1f\\x7f-\\x9f${BIDI_AND_ZERO_WIDTH_CHARS}]{0,128})`, "gu")
    ),
  ];
  const fnMatch = fnMatches.at(-1);
  if (fnMatch) {
    const rawFamily = normalizeFamily(fnMatch[1]);
    if (!rawFamily) {
      result.family = initialFont.family;
    } else {
      result.family = sanitizeFamily(rawFamily) || current.family;
    }
  }

  // \b<0|1|weight> — bold. The pattern relies on `\b` being followed
  // strictly by a digit (the bold-weight value). ASS-spec tags like
  // `\blur<n>` and `\bord<n>` start with `\b` but follow with a letter,
  // so this regex won't false-match them. If a future ASS extension
  // adds a `\b<word>` style tag (e.g. `\bx`), tighten this to
  // `/\\b(\d+)(?![0-9])/` or an explicit word-boundary anchor on the
  // tag name (Round 2 N-R2-12). Currently safe by spec; comment is
  // the contract.
  // W7.5: matchAll().at(-1) for libass parity — see \r above.
  const bMatches = [...block.matchAll(/\\b(\d+)/g)];
  const bMatch = bMatches.at(-1);
  if (bMatch) {
    const val = parseInt(bMatch[1], 10);
    // \b0 = not bold, \b1 = bold, \b700+ = bold by weight
    result.bold = val === 1 || val >= 700;
  }

  // \i<0|1> — italic. W7.5: matchAll().at(-1) for libass parity.
  const iMatches = [...block.matchAll(/\\i(\d+)/g)];
  const iMatch = iMatches.at(-1);
  if (iMatch) {
    result.italic = parseInt(iMatch[1], 10) !== 0;
  }

  return result;
}

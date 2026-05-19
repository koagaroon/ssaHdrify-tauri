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

// Module-scope override-tag regexes. Compiling once (rather than per
// override block) avoids `\p{L}` Unicode-property recompilation cost
// (A-R5-FECHAIN-12).
//
// REFACTOR NOTE (R6 W2 / A-R6-2): the previous design had a separate
// `R_RESET_RE` here that `walkText` used to detect "did this block
// contain a `\r` → reset isDrawing", plus a different `\r` matchAll
// regex inside `applyOverrideTags`. The two regexes disagreed on
// digit-led names and silently broke state (N-R6-1 / A-R6-1, fixed in
// R6 W1). More structurally, the four tag-family matchAll passes
// inside `applyOverrideTags` each picked their own `.at(-1)`
// independently, ignoring relative position between families — so
// `{\fnArial\r}` set family=Arial then style=initial (correct), but
// `{\rStyleA\fn…}` came out STYLE=StyleA + family=… (also correct),
// while `{\fnArial\r}` from a left-to-right libass POV should end at
// font=initialFont. R6 W2 consolidates: a single position-sorted pass
// over all five tag families inside `applyOverrideTags`, returning
// `{font, isDrawing}` together. `R_RESET_RE` is no longer needed —
// the `\r` handler in `applyOverrideTags` does the drawing reset
// inline alongside the style reset.
// R7 W1 A-R7-3: overlong-branch upper bound made explicit. Transitively
// bounded by MAX_DIALOGUE_TEXT_LEN = 1_000_000 upstream; 200_000 leaves
// comfortable headroom while still being a concrete cap reviewers can
// audit without chasing the upstream transitive bound. R8 W2 N-R8-10:
// the R_TAG_RE overlong upper {128,199999} (1 leading char + 199999
// continuation = 200000 total) is set so the total-char ceiling of the
// overlong branch equals FN_TAG_RE's {129,200000} (200000 chars). The
// two used to differ by 1 (R_TAG total 200001 vs FN_TAG total 200000);
// cosmetic but the symmetry is the contract reviewers grep for.
const R_TAG_RE =
  /\\r(?:([\p{L}\p{N}_][\p{L}\p{N}_-]{0,127})?(?![\p{L}\p{N}_-])|[\p{L}\p{N}_][\p{L}\p{N}_-]{128,199999})/gu;
// `\fn` regex constructed dynamically because the exclusion class
// interpolates `BIDI_AND_ZERO_WIDTH_CHARS`. Hoisted to a module-level
// const just like the literal regexes above so the compile cost is
// paid once per process, not per override block.
const FN_CHAR_SET = `[^\\\\}{\\x00-\\x1f\\x7f-\\x9f${BIDI_AND_ZERO_WIDTH_CHARS}]`;
const FN_TAG_RE = new RegExp(
  `\\\\fn(?:(${FN_CHAR_SET}{0,128})(?!${FN_CHAR_SET})|${FN_CHAR_SET}{129,200000})`,
  "gu"
);
// Codex ff5b69f5 (post-R7 W1): the three numeric tag regexes must
// capture the FULL digit run, not a bounded prefix. The R7 W1 attempt
// to bound them to `\d{1,4}` / `\d{1,2}` / `\d{1,4}` introduced
// prefix-truncation divergence from ass-compiler's full numeric parse:
// `\b00700` captured `0070` (weight 70, NOT bold) instead of 700 (bold);
// `\i001` captured `00` (not italic) instead of 1 (italic); `\p00001`
// captured `0000` (drawing OFF) instead of 1 (drawing ON). Each path
// silently mis-attributes embedded fonts vs what libass renders.
//
// The original R7 W1 rationale ("avoids match-object pressure under
// `\b1\b1\b1...` packed into 1 MB ~330k match objects") didn't hold:
// every `\b<N>` produces one match regardless of digit bound, so the
// match-object count is identical with or without the bound; only the
// per-match capture length changes. Higher-layer caps already bound
// resource usage: MAX_CAPTION_TEXT_LEN (64,000 bytes per caption) +
// MAX_FONT_VARIANTS (500) + MAX_CODEPOINTS_PER_VARIANT (65,536) +
// MAX_TOTAL_CODEPOINTS (1,000,000), and `\d+` over a single-char class
// is linear with no backtracking.
//
// Asymmetry note vs R_TAG_RE / FN_TAG_RE: those two ARE bounded with an
// explicit overlong second-alternation branch that consumes overlong
// names rather than truncating — style / family names are strings, so
// length-capping the captured identifier makes sense. Numeric tags are
// integer literals; `\d+` + `parseInt` already handles arbitrary
// length, so the same "bounded + overlong branch" shape is unnecessary
// and the simplest correct form is unbounded `\d+`.
const B_TAG_RE = /\\b(\d+)/g;
const I_TAG_RE = /\\i(\d+)/g;
const P_TAG_RE = /\\p(\d+)/g;

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
 *  re-surfaced after the inline `safeName` regex inside
 *  `buildFontEntry` (ass-uuencode.ts) had already scrubbed it on the
 *  [Fonts] header path). Full parity with that inline regex on the
 *  shared codepoints — see `sanitization.test.ts` for the pin.
 *
 *  Exported for the cross-helper symmetry pin test: the parity claim
 *  between this helper and the inline regex inside
 *  `ass-uuencode::buildFontEntry` (NOT a named export — `safeName` is
 *  a local `const` inside that function; grep "BIDI_AND_ZERO_WIDTH"
 *  to find both consumers) is enforced by a test that exercises both
 *  sides on the same input range. */
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
        // Round 10 N-R10-034: throw on per-variant cap for parity
        // with MAX_FONT_VARIANTS (above) and MAX_TOTAL_CODEPOINTS
        // (below). Pre-R10 this branch silently `break`ed,
        // truncating the variant's glyph set without surfacing the
        // limit — under adversarial input (a crafted ASS that
        // sprays a million unique codepoints into one font) the
        // user would receive a subsetted font missing characters
        // they could see in the source. Aligning to throw makes
        // the cap-hit a hard failure (visible error message) that
        // the user can act on by splitting the input or excluding
        // the offending file.
        throw new Error(
          `Too many codepoints for one font variant: ${usage.codepoints.size}+ (max ${MAX_CODEPOINTS_PER_VARIANT})`
        );
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
    // Round 11 W11.1 (A2-R11-01): throw rather than silently truncate
    // — parity with MAX_FONT_VARIANTS / MAX_CODEPOINTS_PER_VARIANT /
    // MAX_TOTAL_CODEPOINTS (the R10 N-R10-034 precedent). Pre-R11 the
    // slice() form lost glyphs from the font analysis, producing a
    // subsetted font that silently missed characters present in the
    // source dialogue. The cap is 1 MB; legitimate ASS dialogues are
    // 50-500 chars, so hitting it means hostile or corrupt input
    // worth surfacing as a hard error the user can act on.
    throw new Error(`Dialogue text too long: ${text.length}+ (max ${MAX_DIALOGUE_TEXT_LEN})`);
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
          // R7 W1 A-R7-6: one alternation pass instead of three
          // sequential replaces. Each `.replace(...)` allocates a fresh
          // intermediate string; for a 1 MB malformed-brace tail packed
          // with `\N` / `\n` / `\h`, three passes allocated ~3 MB of
          // intermediate strings. Single alternation is semantic-identical.
          const cleanTail = tail.replace(/\\[Nnh]/g, "");
          if (cleanTail.length > 0) recordChars(current, cleanTail);
        }
        return;
      }

      const block = text.slice(i + 1, closeIdx);
      // Position-sorted single-pass override handling (R6 W2 / A-R6-2):
      // applyOverrideTags now processes all five tag families
      // (\r / \fn / \b / \i / \p) in source order and returns both the
      // updated font state AND the new drawing-mode flag. The previous
      // walkText design ran three independent passes — applyOverrideTags
      // (for font), then `R_RESET_RE.test` (drawing reset on \r), then
      // `\p` matchAll (drawing toggle) — which made `{\p1\r}X` drop X
      // because the \p pass ran AFTER the \r reset. The new design
      // walks the block left-to-right and lets each tag take effect
      // in spec order. See REFACTOR NOTE above the regex constants
      // at module top + the WHY block on applyOverrideTags itself.
      const overrideResult = applyOverrideTags(block, current, isDrawing, initialFont, styleMap);
      current = overrideResult.font;
      isDrawing = overrideResult.isDrawing;
      i = closeIdx + 1;
    } else {
      // Plain text — find the next override block or end
      const nextBrace = text.indexOf("{", i);
      const plainEnd = nextBrace >= 0 ? nextBrace : text.length;
      const plain = text.slice(i, plainEnd);

      // Skip ASS drawing commands (\N, \n, \h) and line breaks.
      // R7 W1 A-R7-6: combined alternation (one allocator pass, was
      // three sequential .replace calls) — see the malformed-brace
      // tail path above for the rationale.
      const cleanText = plain.replace(/\\[Nnh]/g, "");

      if (cleanText.length > 0 && !isDrawing) {
        recordChars(current, cleanText);
      }
      i = plainEnd;
    }
  }
}

/**
 * Result of processing one override block. `font` is the new style/family
 * state; `isDrawing` is the new drawing-mode flag (\p<n>/\r combined).
 */
interface OverrideResult {
  font: FontKey;
  isDrawing: boolean;
}

/** Discriminated union of override-tag matches collected across all five
 *  tag-family regexes. `pos` is the match's byte index inside the block,
 *  used to sort tags into source order before applying. */
type OverrideTag =
  | { kind: "r"; pos: number; styleName: string | undefined }
  | { kind: "fn"; pos: number; family: string | undefined }
  | { kind: "b"; pos: number; weight: number }
  | { kind: "i"; pos: number; flag: number }
  | { kind: "p"; pos: number; scale: number };

/**
 * Apply override tags from a single `{ … }` block in libass left-to-right
 * order, returning the new font + drawing-mode state.
 *
 * **Tag-family regex history** (preserved for archeology — git log has
 * the per-commit detail):
 *
 * - `\r` capture-group leading class: `[A-Za-z]` → `[\p{L}_]` (Codex
 *   52379e14: accept CJK / `_Alt` / `José`) → `[\p{L}\p{N}_]` (R5 W1
 *   A-R5-1: accept digit-led style names that ass-compiler stores in
 *   styleMap without validation). The continuation class is
 *   `[\p{L}\p{N}_-]` — leading + continuation now differ only on dash
 *   (dash-at-start is a typo trap). Pattern 3 sub-question 2 lesson
 *   from R4 W1 / R5 W1: when a regex change alters what `matchAll`
 *   returns for some input shape, audit every caller that walks the
 *   matches for state-machine effects, AND audit the ENTIRE
 *   input-shape catalog before declaring the fix complete (one shape
 *   closed is not the whole catalog).
 *
 * - `\r` overlong handling: bare `{0,127}` upper bound + boundary
 *   lookahead `(?![\p{L}\p{N}_-])` (Codex 994c42d1) → SECOND alternation
 *   branch `[\p{L}\p{N}_][\p{L}\p{N}_-]{128,}` matching overlong runs
 *   WITHOUT a capture group (Codex f871d0cc / R4 W1). Without the
 *   second branch, `matchAll` simply skipped overlong tokens, letting
 *   a prior `\r<valid>` in the same block leave its state in force.
 *   With it, overlong matches with undefined capture → falls through
 *   the `styleName && …` check to `font = initialFont`.
 *
 * - `\rdefault`: canonical "reset to dialogue initial" form, handled
 *   by the literal-string check `styleName.toLowerCase() !== "default"`.
 *   This pre-empts a `Style: default,…` definition in [V4+ Styles]
 *   that would otherwise shadow the dialogue's initial style.
 *
 * - `\fn` family capture: sibling-parity with `\r` for the boundary
 *   lookahead and overlong-alternation patterns; same Codex finding
 *   IDs. Exclusion class `[^\\}{C0/DEL/C1/BiDi]` (Round 6 Wave 6.2 +
 *   Round 8 A-R8-A1-3) stops the capture at a literal `{` so
 *   `\fn{Evil}` doesn't carry the brace into match keys, and stops
 *   at control chars so `\fn<U+202E>evil` doesn't make the embed
 *   family diverge from the libass-rendered family.
 *
 * - `\b`: relies on `\b` being followed strictly by a digit; ASS spec
 *   tags `\blur<n>` / `\bord<n>` start with `\b` but follow with a
 *   letter, so the regex won't false-match (Round 2 N-R2-12). If a
 *   future ASS extension adds a `\b<word>` tag, tighten the regex.
 *
 * - **Position-sorted single-pass** (R6 W2 / A-R6-2): the previous
 *   design ran four independent `matchAll().at(-1)` passes (one per
 *   tag family) and the walkText caller ran two more passes for `\r`
 *   reset detection and `\p` drawing-mode toggle. Family-independent
 *   last-wins ignored relative position between families: e.g.
 *   `{\fnArial\r}` would set family=Arial AND style=initial because
 *   the `\fn` pass ran after `\r` and overwrote `result.family`; but
 *   libass / xy-VSFilter process tags left-to-right and `\r` should
 *   reset family back to initialFont. Same shape applied to
 *   `{\p1\r}` (drawing-on then reset → libass renders, our code
 *   dropped glyphs), `{\b1\r}`, `{\i1\r}`. Refactor collects all
 *   five tag families into one position-sorted list and walks them
 *   left-to-right; `\r` resets both font state AND drawing-mode in
 *   the same handler. Tests pinning previous edge-case behavior
 *   (`current.family` fallback in `\fn` sanitize-failure path) are
 *   preserved by referencing the function parameter `current` (the
 *   pre-block snapshot) rather than the running `font` state.
 *
 * - **W7.5 / R7 W7.5 last-wins per family** is preserved by the
 *   sort being stable per family: when the same family appears
 *   twice in one block (`{\fnArial\fnTimes}`), the later match
 *   simply overwrites in the walk and the family-final = last
 *   occurrence.
 */
function applyOverrideTags(
  block: string,
  current: FontKey,
  currentDrawing: boolean,
  initialFont: FontKey,
  styleMap: Map<string, FontKey>
): OverrideResult {
  let font = { ...current };
  let isDrawing = currentDrawing;

  // Collect matches from each tag family into one position-tagged list.
  // `m.index` is reliably set for matchAll results per the JS spec —
  // the optional-typing of `RegExpExecArray.index` in lib.es5.d.ts is
  // a `.match()` non-global concern that doesn't apply to matchAll.
  const tags: OverrideTag[] = [];
  for (const m of block.matchAll(R_TAG_RE)) {
    tags.push({ kind: "r", pos: m.index!, styleName: m[1] });
  }
  for (const m of block.matchAll(FN_TAG_RE)) {
    tags.push({ kind: "fn", pos: m.index!, family: m[1] });
  }
  for (const m of block.matchAll(B_TAG_RE)) {
    tags.push({ kind: "b", pos: m.index!, weight: parseInt(m[1]!, 10) });
  }
  for (const m of block.matchAll(I_TAG_RE)) {
    tags.push({ kind: "i", pos: m.index!, flag: parseInt(m[1]!, 10) });
  }
  for (const m of block.matchAll(P_TAG_RE)) {
    tags.push({ kind: "p", pos: m.index!, scale: parseInt(m[1]!, 10) });
  }

  // Stable position sort — preserves insertion order on tie, though
  // ties between different tag families are structurally impossible
  // (all start with `\` followed by a distinguishing letter).
  tags.sort((a, b) => a.pos - b.pos);

  for (const tag of tags) {
    switch (tag.kind) {
      case "r": {
        const styleName = tag.styleName;
        if (styleName && styleName.toLowerCase() !== "default" && styleMap.has(styleName)) {
          font = { ...styleMap.get(styleName)! };
        } else {
          font = { ...initialFont };
        }
        // libass: `\r` resets ALL style state including drawing mode.
        // Previous walkText design did this via a separate
        // `R_RESET_RE.test(block)` pass, which the `\p` pass then
        // overwrote on `{\p1\r}` (A-R6-2). Folded into the `\r`
        // handler here so the position-sorted walk gets it right.
        isDrawing = false;
        break;
      }
      case "fn": {
        // `\fn<empty>` and the overlong second-alternation branch
        // both produce `tag.family === undefined`; the `?? ""` keeps
        // the existing fall-through-to-initialFont semantic.
        //
        // R7 W1 (N-R7-1 / A-R7-2 / A-R7-5): the previous
        // `sanitizeFamily(rawFamily) || current.family` fallback was
        // STRUCTURALLY UNREACHABLE. FN_CHAR_SET excludes the same
        // codepoints `sanitizeFamily` strips (C0 / DEL / C1 / BiDi /
        // zero-width) plus `\` `{` `}`, AND the first FN_TAG_RE
        // alternation branch caps at 128 chars — exactly what
        // `sanitizeFamily` truncates to. So once `rawFamily` is
        // non-empty after `normalizeFamily`, `sanitizeFamily(rawFamily)`
        // is byte-equal to `rawFamily` itself and never returns "".
        // The `|| current.family` clause could only fire if FN_CHAR_SET
        // were ever loosened to admit chars `sanitizeFamily` strips —
        // in that future scenario, the right reference frame is
        // ambiguous (running font vs pre-block snapshot vs initial)
        // and would need libass verification at that time. Drop the
        // dead fallback today; the post-R6-W2 position-sorted walk
        // doesn't need it.
        const rawFamily = normalizeFamily(tag.family ?? "");
        if (!rawFamily) {
          font.family = initialFont.family;
        } else {
          font.family = sanitizeFamily(rawFamily);
        }
        break;
      }
      case "b":
        // `\b0` = not bold; `\b1` = bold; `\b2`-`\b699` = libass treats
        // as not-bold (only `1` is the bold-on flag in the low range);
        // `\b700+` = bold by font weight value (CSS-style weight scale).
        // R7 W1 N-R7-12: middle range named explicitly so the contract
        // matches the predicate. B_TAG_RE captures the full digit run
        // (see Codex ff5b69f5 WHY block at the regex const); overlong
        // values like `\b00700` parse as 700 → bold-on per libass.
        font.bold = tag.weight === 1 || tag.weight >= 700;
        break;
      case "i":
        font.italic = tag.flag !== 0;
        break;
      case "p":
        // libass: positive scale = drawing-on, zero = drawing-off.
        // libass parses the full numeric value; treat any positive
        // integer as drawing-on.
        isDrawing = tag.scale > 0;
        break;
    }
  }

  return { font, isDrawing };
}

import { SECTION_HEADER_RE } from "../hdr-convert/ass-processor";
import { MAX_PARSED_ENTRIES } from "../../lib/subtitle-parser";

/// Shared shape + paired size/line guard reused by `embedFonts`
/// upfront AND `insertFontsSection` at its boundary. Direct callers of
/// `insertFontsSection` (`cli-engine-entry.ts::applyFontEmbed`)
/// bypass `processAssContent`'s upstream byte+line paired guard —
/// without a helper-layer backstop, hostile content reaching this
/// surface hits unbounded `split(/\r?\n/)` allocation.
///
/// The line-count probe is the paired Pattern 2 fix. Without it, the
/// byte cap would stand alone: a 50 MB pure-newline blob passes the
/// 100 MB byte gate but then `.split(/\r?\n/)` at line ~660 below
/// allocates ~50M empty strings (~2 GB V8 heap) BEFORE any downstream
/// throw can fire. Mirrors ass-processor.ts:282-306's paired cap;
/// `MAX_INSERT_LINES` derived from the same MAX_PARSED_ENTRIES +
/// header-budget basis so an SRT→ASS upcast that parseSrt accepted
/// can still re-pass through embed.
const MAX_INSERT_FONTS_SECTION_CONTENT = 100_000_000;
const INSERT_FONTS_SECTION_HEADER_BUDGET = 1024;
const MAX_INSERT_LINES = MAX_PARSED_ENTRIES + INSERT_FONTS_SECTION_HEADER_BUDGET;
const LINE_PROBE_BYTE_GATE = 1_000_000;

// Module-scope to match the project convention (sibling
// SRT_COLOR_*_RE / WHITESPACE_RE). Anchored at column 0 and trailing
// whitespace restricted to ASCII space/tab only —
// plain `\s*` would also match U+2028 / U+2029, letting a crafted ASS
// with `[FONTS]\u2028` on one line still match the header regex. This
// closes the false-positive hole that `.trim().toLowerCase()` left
// open AND blocks the Unicode-line-sep smuggle.
const HEADER_FONTS_RE = /^\[[Ff][Oo][Nn][Tt][Ss]\][ \t]*$/;
const HEADER_EVENTS_RE = /^\[[Ee][Vv][Ee][Nn][Tt][Ss]\][ \t]*$/;

export function assertAssShape(content: string): void {
  if (content.length > MAX_INSERT_FONTS_SECTION_CONTENT) {
    throw new Error(`File too large: ${(content.length / 1_000_000).toFixed(1)} MB (max 100 MB)`);
  }
  // Pre-split line-count probe (mirror of ass-processor.ts:286-306).
  // Gated on content.length to keep the
  // small-file fast path zero-overhead. The 1 MB gate is well above
  // realistic small subtitles (5-200 KB) and well below the attack
  // threshold (tens of MB). Count every separator that
  // insertFontsSection normalizes into an ASCII newline before
  // splitting, otherwise U+2028 / U+2029 can bypass the guard.
  if (content.length > LINE_PROBE_BYTE_GATE) {
    let nl = 1;
    for (let i = 0; i < content.length; i++) {
      const code = content.charCodeAt(i);
      if (code === 10 /* '\n' */ || code === 0x2028 || code === 0x2029) {
        nl++;
        if (nl > MAX_INSERT_LINES) {
          throw new Error(`File too large: >${MAX_INSERT_LINES} lines`);
        }
      }
    }
  }
  if (!/^\[Script Info\][ \t]*$/im.test(content)) {
    throw new Error(
      "Cannot embed: input ASS has no [Script Info] section header. " +
        "Re-parse / rebuild the file before embedding fonts."
    );
  }
}

/**
 * Insert [Fonts] section into ASS content.
 * Position: after [V4+ Styles], before [Events].
 * If [Fonts] already exists, replace it.
 */
export function insertFontsSection(content: string, fontsSection: string): string {
  // Defense-in-depth at the helper boundary. `processAssContent`'s
  // 100 MB byte guard upstream covers the standalone HDR + chain
  // paths, but `cli-engine-entry.ts::applyFontEmbed` (standalone embed
  // CLI flow) calls `insertFontsSection` directly on caller-supplied
  // content — without a per-callsite cap or this helper-layer
  // backstop, a hostile pack with a multi-hundred-MB ASS hits
  // unbounded `split(/\r?\n/)` allocation here. Routed through the
  // shared `assertAssShape` helper so embedFonts (zero-font
  // early-return) and direct callers run the same gate.
  assertAssShape(content);
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
  // HEADER_FONTS_RE / HEADER_EVENTS_RE hoisted to module scope.
  // Reject malformed input with multiple [Fonts] sections. Mirrors the
  // CLI's identical guard in cli-engine-entry.ts: the replace path
  // below only rewrites the first occurrence; silently leaving extra
  // sections in place would produce a corrupted ASS with conflicting
  // font data. Pre-Round-3 fix the GUI accepted these.
  const fontsHeaderIndices: number[] = [];
  for (let i = 0; i < lines.length; i += 1) {
    if (HEADER_FONTS_RE.test(lines[i]!)) fontsHeaderIndices.push(i);
  }
  if (fontsHeaderIndices.length > 1) {
    throw new Error(
      `Cannot embed: input ASS has ${fontsHeaderIndices.length} [Fonts] sections; expected at most one`
    );
  }
  const existingFontsIdx = fontsHeaderIndices[0] ?? -1;

  // Build "before" from a line slice: strip trailing blank lines so we control
  // the separator ourselves. Array.join() absorbs trailing "" elements into a
  // single lineEnding, making blank separator lines invisible — so we strip them
  // and add an explicit blank-line separator instead.
  const buildBefore = (endIdx: number): { text: string; sep: string } => {
    const slice = lines.slice(0, endIdx);
    while (slice.length > 0 && slice[slice.length - 1]!.trim() === "") {
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
    while (slice.length > 0 && slice[0]!.trim() === "") {
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
    while (endIdx < lines.length && !isSectionHeader(lines[endIdx]!)) {
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

  // No existing [Fonts] — insert before [Events]. HEADER_EVENTS_RE
  // shares the column-0 + ASCII-space-only shape with HEADER_FONTS_RE
  // for the same UUEncode-false-positive + Unicode-line-sep reasons
  // (see module-scope definitions).
  const eventsHeaderIndices: number[] = [];
  for (let i = 0; i < lines.length; i += 1) {
    if (HEADER_EVENTS_RE.test(lines[i]!)) eventsHeaderIndices.push(i);
  }
  // parity with the [Fonts]-duplicate reject above.
  // A single ASS with two [Events] sections produces a corrupted file
  // either way — libass reads only the first, every other consumer
  // (Aegisub / mpv) may pick the second — and inserting [Fonts] before
  // the FIRST [Events] silently leaves the second's dialogues intact
  // downstream. Surface the malformed input the same way [Fonts]
  // duplicates are surfaced.
  if (eventsHeaderIndices.length > 1) {
    throw new Error(
      `Cannot embed: input ASS has ${eventsHeaderIndices.length} [Events] sections; expected at most one`
    );
  }
  const eventsIdx = eventsHeaderIndices[0] ?? -1;

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

import { MAX_PARSED_ENTRIES } from "../../lib/subtitle-parser";

/// Shared shape + paired size/line guard reused by `embedFonts`
/// upfront AND `insertFontsSection` at its boundary. Direct callers of
/// `insertFontsSection` (`cli-engine-entry.ts::applyFontEmbed`)
/// bypass `processAssContent`'s upstream byte+line paired guard —
/// without a helper-layer backstop, hostile content reaching this
/// surface hits unbounded line allocation.
const MAX_INSERT_FONTS_SECTION_CONTENT = 100_000_000;
const INSERT_FONTS_SECTION_HEADER_BUDGET = 1024;
const MAX_INSERT_LINES = MAX_PARSED_ENTRIES + INSERT_FONTS_SECTION_HEADER_BUDGET;
const LINE_PROBE_LENGTH_GATE = MAX_INSERT_LINES;

const HEADER_FONTS_RE = /^\[[Ff][Oo][Nn][Tt][Ss]\][ \t]*$/;
const HEADER_EVENTS_RE = /^\[[Ee][Vv][Ee][Nn][Tt][Ss]\][ \t]*$/;
const HEADER_SCRIPT_INFO_RE = /^(?:\uFEFF)?\[Script Info\][ \t]*$/im;

// UUEncode emits ASCII 33..96 only (never space 32 or lowercase 97..122).
// Testing raw `trimmed` for `[a-z ]` prevents short uppercase UUEncode tail
// lines like `[A]` or `[ABCD12+]` from terminating `[Fonts]` replacement early,
// while KNOWN_SECTION_HEADERS_LOWER still matches all-uppercase standard ASS
// section headers such as `[EVENTS]` or `[GRAPHICS]`.
const RAW_SECTION_HEADER_RE = /^\[(?=[A-Za-z0-9+ ]*[a-z ])[A-Za-z0-9+ ]{1,50}\]$/;
const KNOWN_SECTION_HEADERS_LOWER = new Set([
  "[script info]",
  "[v4+ styles]",
  "[v4 styles]",
  "[fonts]",
  "[events]",
  "[graphics]",
]);

function isEmbedSectionBoundaryHeader(line: string): boolean {
  const trimmed = line.trim();
  if (!trimmed.startsWith("[") || !trimmed.endsWith("]")) return false;
  return (
    KNOWN_SECTION_HEADERS_LOWER.has(trimmed.toLowerCase()) || RAW_SECTION_HEADER_RE.test(trimmed)
  );
}

interface SectionLineSpan {
  start: number;
  bodyEnd: number;
  headerText: string;
}

function scanSectionLineSpans(content: string): SectionLineSpan[] {
  const spans: SectionLineSpan[] = [];
  let lineStart = 0;
  for (let i = 0; i <= content.length; i += 1) {
    const code = i < content.length ? content.charCodeAt(i) : 10;
    if (
      i < content.length &&
      code !== 13 /* '\r' */ &&
      code !== 10 /* '\n' */ &&
      code !== 0x2028 &&
      code !== 0x2029
    ) {
      continue;
    }

    const headerStart = lineStart === 0 && content.charCodeAt(0) === 0xfeff ? 1 : lineStart;
    spans.push({
      start: lineStart,
      bodyEnd: i,
      headerText: content.slice(headerStart, i),
    });

    if (code === 13 /* '\r' */ && content.charCodeAt(i + 1) === 10 /* '\n' */) {
      i += 1;
    }
    lineStart = i + 1;
  }
  return spans;
}

function assertUniqueEmbedSections(content: string): void {
  let fonts = 0;
  let events = 0;
  let lineStart = 0;
  for (let i = 0; i <= content.length; i++) {
    const code = i < content.length ? content.charCodeAt(i) : 10;
    if (
      i < content.length &&
      code !== 13 /* '\r' */ &&
      code !== 10 /* '\n' */ &&
      code !== 0x2028 &&
      code !== 0x2029
    ) {
      continue;
    }

    const headerStart = lineStart === 0 && content.charCodeAt(0) === 0xfeff ? 1 : lineStart;
    if (content.charCodeAt(headerStart) === 91 /* '[' */) {
      const line = content.slice(headerStart, i);
      if (HEADER_FONTS_RE.test(line)) {
        fonts += 1;
        if (fonts > 1) {
          throw new Error(
            `Cannot embed: input ASS has ${fonts} [Fonts] sections; expected at most one`
          );
        }
      } else if (HEADER_EVENTS_RE.test(line)) {
        events += 1;
        if (events > 1) {
          throw new Error(
            `Cannot embed: input ASS has ${events} [Events] sections; expected at most one`
          );
        }
      }
    }
    if (code === 13 /* '\r' */ && content.charCodeAt(i + 1) === 10 /* '\n' */) {
      i += 1;
    }
    lineStart = i + 1;
  }
}

export function assertAssShape(content: string): void {
  if (content.length > MAX_INSERT_FONTS_SECTION_CONTENT) {
    throw new Error(`File too large: ${(content.length / 1_000_000).toFixed(1)} MB (max 100 MB)`);
  }
  if (content.length >= LINE_PROBE_LENGTH_GATE) {
    let nl = 1;
    for (let i = 0; i < content.length; i++) {
      const code = content.charCodeAt(i);
      if (code === 13 /* '\r' */ || code === 10 /* '\n' */ || code === 0x2028 || code === 0x2029) {
        nl++;
        if (nl > MAX_INSERT_LINES) {
          throw new Error(`File too large: >${MAX_INSERT_LINES} lines`);
        }
        if (code === 13 /* '\r' */ && content.charCodeAt(i + 1) === 10 /* '\n' */) {
          i += 1;
        }
      }
    }
  }
  if (!HEADER_SCRIPT_INFO_RE.test(content)) {
    throw new Error(
      "Cannot embed: input ASS has no [Script Info] section header. " +
        "Re-parse / rebuild the file before embedding fonts."
    );
  }
  assertUniqueEmbedSections(content);
}

function detectPreferredLineEnding(content: string): string {
  if (content.includes("\r\n")) return "\r\n";
  if (content.includes("\n")) return "\n";
  if (content.includes("\r")) return "\r";
  return "\n";
}

/**
 * Insert [Fonts] section into ASS content.
 * Position: after [V4+ Styles], before [Events].
 * If [Fonts] already exists, replace it while keeping untouched sections'
 * exact source bytes and line endings intact.
 */
export function insertFontsSection(content: string, fontsSection: string): string {
  assertAssShape(content);
  const lineEnding = detectPreferredLineEnding(content);
  const adaptedFontsSection = fontsSection.replace(/\n/g, lineEnding);
  const spans = scanSectionLineSpans(content);

  const fontsHeaderIndices: number[] = [];
  const eventsHeaderIndices: number[] = [];
  for (let i = 0; i < spans.length; i += 1) {
    const headerText = spans[i]!.headerText;
    if (HEADER_FONTS_RE.test(headerText)) fontsHeaderIndices.push(i);
    if (HEADER_EVENTS_RE.test(headerText)) eventsHeaderIndices.push(i);
  }
  if (fontsHeaderIndices.length > 1) {
    throw new Error(
      `Cannot embed: input ASS has ${fontsHeaderIndices.length} [Fonts] sections; expected at most one`
    );
  }
  if (eventsHeaderIndices.length > 1) {
    throw new Error(
      `Cannot embed: input ASS has ${eventsHeaderIndices.length} [Events] sections; expected at most one`
    );
  }
  const existingFontsIdx = fontsHeaderIndices[0] ?? -1;
  const eventsIdx = eventsHeaderIndices[0] ?? -1;

  const buildBefore = (endIdx: number): { text: string; sep: string } => {
    let lastNonBlank = endIdx - 1;
    while (lastNonBlank >= 0 && spans[lastNonBlank]!.headerText.trim() === "") {
      lastNonBlank -= 1;
    }
    if (lastNonBlank < 0) {
      return { text: content.startsWith("\uFEFF") ? "\uFEFF" : "", sep: "" };
    }
    return {
      text: content.slice(0, spans[lastNonBlank]!.bodyEnd),
      sep: lineEnding + lineEnding,
    };
  };

  const buildAfter = (startIdx: number): string => {
    let firstNonBlank = startIdx;
    while (firstNonBlank < spans.length && spans[firstNonBlank]!.headerText.trim() === "") {
      firstNonBlank += 1;
    }
    return firstNonBlank < spans.length ? content.slice(spans[firstNonBlank]!.start) : "";
  };

  if (existingFontsIdx >= 0) {
    let endIdx = existingFontsIdx + 1;
    while (endIdx < spans.length && !isEmbedSectionBoundaryHeader(spans[endIdx]!.headerText)) {
      endIdx += 1;
    }

    const { text: before, sep } = buildBefore(existingFontsIdx);
    const after = buildAfter(endIdx);
    const afterSep = after.length > 0 ? lineEnding : "";
    return `${before}${sep}${adaptedFontsSection}${afterSep}${after}`;
  }

  if (eventsIdx >= 0) {
    const { text: before, sep } = buildBefore(eventsIdx);
    const after = content.slice(spans[eventsIdx]!.start);
    return `${before}${sep}${adaptedFontsSection}${lineEnding}${after}`;
  }

  const { text: before } = buildBefore(spans.length);
  return `${before}${lineEnding}${lineEnding}${adaptedFontsSection}`;
}

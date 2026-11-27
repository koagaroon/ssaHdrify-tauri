import { decodeBase64Bytes } from "../../lib/base64-bytes";
import { sanitizeError } from "../../lib/dedup-helpers";
import {
  ASCII_CONTROL_CHARS,
  BIDI_AND_ZERO_WIDTH_CHARS,
  stripUnicodeControls,
} from "../../lib/unicode-controls";

/**
 * ASS-specific UUEncode implementation.
 *
 * The ASS [Fonts] section uses a custom binary encoding that is NOT
 * standard UUEncode. The algorithm:
 *
 * 1. Take 3 bytes at a time
 * 2. Split into four 6-bit values
 * 3. Add 33 (0x21) to each to get a printable ASCII character
 * 4. For the final partial group, emit only `remaining + 1` characters
 *    (1 trailing byte -> 2 chars, 2 trailing bytes -> 3 chars); a full
 *    3-byte group emits 4
 * 5. Output lines of max 80 characters
 *
 * Step 4 is load-bearing, not cosmetic: assfonts, Aegisub, and VSFilter all
 * emit `remaining + 1` chars for the tail, and libass reconstructs the byte
 * count as `size/4*3 + max(size%4, 1) - 1`. Emitting a full 4 chars for a
 * partial tail keeps the total a multiple of 4, so libass does NOT reject it
 * (it only errors on `size%4 == 1`) — it silently appends 1-2 NUL bytes to
 * the decoded font. Such a font still renders (FreeType ignores trailing
 * bytes), but the embedded payload is no longer byte-identical to assfonts /
 * Aegisub output. Matching `remaining + 1` keeps byte-parity and round-trips
 * exactly through libass.
 *
 * This matches Aegisub's implementation and is compatible with all
 * major ASS renderers (libass, VSFilter, xy-VSFilter).
 */

/**
 * Independent TypeScript encoder cap. Rust permits source font files up to
 * 64 MiB and separately caps each command's aggregate subset payload; this
 * guard limits the later string-building allocation even when a caller
 * bypasses those layers. 50 MiB of input yields about 67 MiB of encoded text
 * plus the `fontname:` header overhead — well past any realistic subset.
 */
export const MAX_FONT_DATA_SIZE = 50 * 1024 * 1024;
export const MAX_ENCODED_FONT_SECTION_BYTES = 50 * 1024 * 1024;

function safeFontEntryName(fontName: string): string {
  if (fontName.length > 1024) throw new Error("Font entry name exceeds 1024 characters");
  return fontName.replace(
    new RegExp(`[${ASCII_CONTROL_CHARS}${BIDI_AND_ZERO_WIDTH_CHARS}:/\\\\]`, "gu"),
    "_"
  );
}

function utf8ByteLength(text: string): number {
  let bytes = 0;
  for (const character of text) {
    const point = character.codePointAt(0)!;
    bytes += point < 0x80 ? 1 : point < 0x800 ? 2 : point < 0x10000 ? 3 : 4;
  }
  return bytes;
}

function encodedBodyByteLength(dataLength: number): number {
  if (!Number.isSafeInteger(dataLength) || dataLength <= 0 || dataLength > MAX_FONT_DATA_SIZE) {
    throw new Error(`Invalid font data size: ${dataLength} (max ${MAX_FONT_DATA_SIZE})`);
  }
  const remainder = dataLength % 3;
  const encodedChars = Math.floor(dataLength / 3) * 4 + (remainder ? remainder + 1 : 0);
  return encodedChars + Math.floor((encodedChars - 1) / 80);
}

function fontEntryByteLengthForSafeName(safeName: string, dataLength: number): number {
  // The CLI's bare V8 runtime has no TextEncoder or Buffer. Lone UTF-16
  // surrogates occupy three bytes after UTF-8 replacement, like U+FFFD.
  return 11 + utf8ByteLength(safeName) + encodedBodyByteLength(dataLength);
}

/** Exact UTF-8 size, including the header and 80-column line separators. */
export function fontEntryByteLength(fontName: string, dataLength: number): number {
  return fontEntryByteLengthForSafeName(safeFontEntryName(fontName), dataLength);
}

function disambiguateSafeFontName(safeName: string, usedLowerNames: ReadonlySet<string>): string {
  if (!usedLowerNames.has(safeName.toLowerCase())) {
    return safeName;
  }
  const lower = safeName.toLowerCase();
  const hasTtfSuffix = lower.endsWith(".ttf");
  const stem = hasTtfSuffix ? safeName.slice(0, -4) : safeName;
  const ext = hasTtfSuffix ? safeName.slice(-4) : "";
  let counter = 2;
  while (true) {
    const candidate = `${stem}_${counter}${ext}`;
    if (!usedLowerNames.has(candidate.toLowerCase())) {
      return candidate;
    }
    counter += 1;
  }
}

/** Reject cumulative growth before allocating the next encoded entry. */
export class FontSectionBuilder {
  private entries: string[] = [];
  private bytes = 9; // [Fonts] header and final newline.
  private readonly maxBytes: number;
  private readonly usedNames = new Set<string>();

  constructor(maxBytes = MAX_ENCODED_FONT_SECTION_BYTES) {
    this.maxBytes = maxBytes;
  }

  get count(): number {
    return this.entries.length;
  }

  add(fontName: string, data: Uint8Array): void {
    const uniqueSafeName = disambiguateSafeFontName(safeFontEntryName(fontName), this.usedNames);
    const projected =
      this.bytes +
      (this.count ? 2 : 0) +
      fontEntryByteLengthForSafeName(uniqueSafeName, data.length);
    if (projected > this.maxBytes) {
      throw new Error(
        `Embedded fonts exceed the ${this.maxBytes}-byte encoded output limit; select fewer fonts`
      );
    }
    const entry = buildFontEntryFromSafeName(uniqueSafeName, data);
    this.usedNames.add(uniqueSafeName.toLowerCase());
    this.entries.push(entry);
    this.bytes = projected;
  }

  build(): string {
    return this.count ? `[Fonts]\n${this.entries.join("\n\n")}\n` : "";
  }
}

/**
 * Encode a binary buffer into ASS [Fonts] section format.
 *
 * @param data - Binary font data (Uint8Array)
 * @returns Array of encoded lines (without the fontname: header)
 */
export function assUuencode(data: Uint8Array): string[] {
  if (data.length > MAX_FONT_DATA_SIZE) {
    throw new Error(`Font data too large: ${data.length} bytes (max ${MAX_FONT_DATA_SIZE})`);
  }
  const lines: string[] = [];
  let currentLine = "";

  for (let i = 0; i < data.length; i += 3) {
    // Bytes remaining in this (possibly partial) group: 3 for a full group,
    // 1 or 2 for the final tail. Missing bytes are read as 0 (padding).
    const remaining = data.length - i;
    const b0 = data[i]!;
    const b1 = i + 1 < data.length ? data[i + 1]! : 0;
    const b2 = i + 2 < data.length ? data[i + 2]! : 0;

    // Split 24 bits into four 6-bit values and add 33. Each 6-bit value is
    // in [0, 63], so the encoded char is in [33, 96] by construction —
    // exactly the ASS [Fonts] section's printable-ASCII alphabet. No
    // post-encode validation needed because the arithmetic itself bounds
    // the output range.
    const enc = [
      (b0 >> 2) + 33,
      (((b0 & 0x03) << 4) | (b1 >> 4)) + 33,
      (((b1 & 0x0f) << 2) | (b2 >> 6)) + 33,
      (b2 & 0x3f) + 33,
    ];

    // Emit `remaining + 1` chars for the final partial group (1 byte -> 2,
    // 2 bytes -> 3); a full group emits 4. WHY: this is the assfonts /
    // Aegisub / libass tail contract — emitting 4 for a partial tail appends
    // 1-2 NUL bytes to the decoded font and breaks byte-parity (see the file
    // header comment). One char is appended at a time so the 80-char flush
    // below triggers at exactly 80.
    const count = Math.min(remaining + 1, 4);
    for (let k = 0; k < count; k++) {
      currentLine += String.fromCharCode(enc[k]!);
      if (currentLine.length >= 80) {
        lines.push(currentLine.slice(0, 80));
        currentLine = currentLine.slice(80);
      }
    }
  }

  // Flush remaining
  if (currentLine.length > 0) {
    lines.push(currentLine);
  }

  return lines;
}

/**
 * Build a complete [Fonts] section entry for one font.
 *
 * @param fontName - Display name for the font (e.g., "arial_bold.ttf")
 * @param data - Binary font data (Uint8Array)
 * @returns Complete entry text including "fontname:" header
 */
function buildFontEntryFromSafeName(safeName: string, data: Uint8Array): string {
  if (data.length === 0) {
    throw new Error("Cannot build a [Fonts] entry from empty font data");
  }
  const encodedLines = assUuencode(data);
  return `fontname: ${safeName}\n${encodedLines.join("\n")}`;
}

export function buildFontEntry(fontName: string, data: Uint8Array): string {
  // Strip ALL control chars (C0 + C1), Unicode line separators, plus ':' —
  // the header line is `fontname: <name>`, so a name containing `:` could
  // break line parsing, and a `\u2028` could smuggle a line break into the
  // [Fonts] section. Sibling cross-ref: `srt-converter.ts::FONT_NAME_SANITIZER`
  // adds `,{}\\:` for ASS Style CSV fields, while this boundary adds `:/\\`.
  const safeName = safeFontEntryName(fontName);
  return buildFontEntryFromSafeName(safeName, data);
}

/**
 * Decode base64-encoded font subset bytes with sanitized font-name attribution.
 * Shared by standalone CLI `applyFontEmbed` and `chain` `embedTransform`.
 */
export function decodeSubsetBase64(dataB64: string, name: string): Uint8Array {
  try {
    return decodeBase64Bytes(dataB64);
  } catch (e) {
    const message = sanitizeError(e);
    const safeName = stripUnicodeControls(name);
    throw new Error(`base64 decode failed for font subset '${safeName}': ${message}`, {
      cause: e,
    });
  }
}

import { ASCII_CONTROL_CHARS, BIDI_AND_ZERO_WIDTH_CHARS } from "../../lib/unicode-controls";

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
 * Hard cap on input size. The Rust backend already refuses font files over
 * 50 MB; this guard defends the encoder itself from being handed oversized
 * data through a bypass or refactor. 50 MB of input yields ~67 MB of encoded
 * text plus the `fontname:` header overhead — well past any realistic subset.
 */
export const MAX_FONT_DATA_SIZE = 50 * 1024 * 1024;

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
export function buildFontEntry(fontName: string, data: Uint8Array): string {
  // A [Fonts] entry needs body lines; a 0-byte subset would emit a bare
  // `fontname:` header with no payload — a malformed embedded entry. This
  // can't happen today (subset_font returns Err, never empty bytes, on a
  // real font), but the guard fails loud at the encoder boundary against a
  // bypass or refactor, matching the MAX_FONT_DATA_SIZE defense above.
  if (data.length === 0) {
    throw new Error("Cannot build a [Fonts] entry from empty font data");
  }
  // Strip ALL control chars (C0 + C1), Unicode line separators, plus ':' —
  // the header line is `fontname: <name>`, so a name containing `:` could
  // break line parsing, and a `\u2028` could smuggle a line break into the
  // [Fonts] section. Defense-in-depth: font-embedder.buildFontFileName
  // already sanitizes upstream, but this keeps the encoder self-contained.
  // BiDi + zero-width + line/paragraph separators come from the shared
  // unicode-controls set (mirrors validate_font_family + safeFontName +
  // sanitizeForDialog). Slashes/backslashes are added beyond the
  // upstream buildFontFileName output as a self-contained defense at
  // the encoder boundary.
  //
  // `no-control-regex`
  // eslint-disable directive removed \u2014 the rule only inspects regex
  // literals, and this regex is built via `new RegExp(...)` from a
  // string interpolation, so the rule was never going to fire.
  //
  // Sibling cross-ref: the extra `:/\\` literals on top of the BIDI /
  // control set are SPECIFIC to the [Fonts] header line shape \u2014 header
  // lines are `fontname: <name>` so a name carrying `:` would break the
  // parser, and `/` / `\` get sanitized as a defense-in-depth measure
  // beyond the upstream buildFontFileName sanitizer. The sibling
  // sanitizer in `src/features/hdr-convert/srt-converter.ts::
  // FONT_NAME_SANITIZER` adds `,{}\\:` instead \u2014 that's the ASS
  // Style-line CSV path's stop chars.
  //
  // BOTH sanitizers share `\\` and `:` \u2014 that is the structural
  // overlap; `,{}` are unique to srt-converter side, `/` is unique to
  // this side. Both sanitizers must keep stripping the shared BIDI /
  // control set; the extra boundary-specific chars are
  // intentional and MUST NOT be unified into a single helper without
  // re-checking the per-boundary character implications.
  const safeName = fontName.replace(
    new RegExp(`[${ASCII_CONTROL_CHARS}${BIDI_AND_ZERO_WIDTH_CHARS}:/\\\\]`, "gu"),
    "_"
  );
  const encodedLines = assUuencode(data);
  return `fontname: ${safeName}\n${encodedLines.join("\n")}`;
}

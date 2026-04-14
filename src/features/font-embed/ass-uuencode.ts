/**
 * ASS-specific UUEncode implementation.
 *
 * The ASS [Fonts] section uses a custom binary encoding that is NOT
 * standard UUEncode. The algorithm:
 *
 * 1. Take 3 bytes at a time
 * 2. Split into four 6-bit values
 * 3. Add 33 (0x21) to each to get a printable ASCII character
 * 4. Output lines of max 80 characters
 *
 * This matches Aegisub's implementation and is compatible with all
 * major ASS renderers (libass, VSFilter, xy-VSFilter).
 */

/**
 * Encode a binary buffer into ASS [Fonts] section format.
 *
 * @param data - Binary font data (Uint8Array)
 * @returns Array of encoded lines (without the fontname: header)
 */
export function assUuencode(data: Uint8Array): string[] {
  const lines: string[] = [];
  let currentLine = "";

  for (let i = 0; i < data.length; i += 3) {
    // Read up to 3 bytes, pad missing bytes with 0
    const b0 = data[i];
    const b1 = i + 1 < data.length ? data[i + 1] : 0;
    const b2 = i + 2 < data.length ? data[i + 2] : 0;

    // Split 24 bits into four 6-bit values and add 33
    const c0 = String.fromCharCode((b0 >> 2) + 33);
    const c1 = String.fromCharCode((((b0 & 0x03) << 4) | (b1 >> 4)) + 33);
    const c2 = String.fromCharCode((((b1 & 0x0f) << 2) | (b2 >> 6)) + 33);
    const c3 = String.fromCharCode((b2 & 0x3f) + 33);

    currentLine += c0 + c1 + c2 + c3;

    // Flush at 80 characters
    if (currentLine.length >= 80) {
      lines.push(currentLine.slice(0, 80));
      currentLine = currentLine.slice(80);
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
  const safeName = fontName.replace(/[\r\n]/g, "_");
  const encodedLines = assUuencode(data);
  return `fontname: ${safeName}\n${encodedLines.join("\n")}`;
}

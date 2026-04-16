/**
 * UUEncode verification test script.
 *
 * Takes an ASS file + font files, embeds fonts using our UUEncode algorithm,
 * and writes the result for playback comparison in mpv/MPC-HC.
 *
 * Usage: node scripts/uuencode-test.mjs <ass-file> <font1> [font2] [font3] ...
 * Output: <ass-file-stem>.embedded.ass in the same directory
 */

import { readFileSync, writeFileSync } from "fs";
import { basename, dirname, join } from "path";

// ── UUEncode (same algorithm as ass-uuencode.ts) ─────────

function uuencode(data) {
  const lines = [];
  let line = "";

  for (let i = 0; i < data.length; i += 3) {
    const b0 = data[i] ?? 0;
    const b1 = data[i + 1] ?? 0;
    const b2 = data[i + 2] ?? 0;

    const remaining = data.length - i;

    const c0 = String.fromCharCode((b0 >> 2) + 33);
    const c1 = String.fromCharCode((((b0 & 0x03) << 4) | (b1 >> 4)) + 33);
    const c2 =
      remaining > 1
        ? String.fromCharCode((((b1 & 0x0f) << 2) | (b2 >> 6)) + 33)
        : "";
    const c3 = remaining > 2 ? String.fromCharCode((b2 & 0x3f) + 33) : "";

    line += c0 + c1 + c2 + c3;

    if (line.length >= 80) {
      lines.push(line.slice(0, 80));
      line = line.slice(80);
    }
  }

  if (line.length > 0) {
    lines.push(line);
  }

  return lines.join("\n");
}

function buildFontEntry(fontName, data) {
  const safeName = fontName.replace(/[\r\n]/g, "");
  const encoded = uuencode(data);
  return `fontname: ${safeName}\n${encoded}`;
}

// ── Main ─────────────────────────────────────────────────

const args = process.argv.slice(2);
if (args.length < 2) {
  console.error("Usage: node uuencode-test.mjs <ass-file> <font1> [font2] ...");
  process.exit(1);
}

const assPath = args[0];
const fontPaths = args.slice(1);

// Read ASS file
const assContent = readFileSync(assPath, "utf-8");

// Build [Fonts] section entries
const fontEntries = [];
for (const fp of fontPaths) {
  const fontData = readFileSync(fp);
  const fontName = basename(fp);
  fontEntries.push(buildFontEntry(fontName, fontData));
  console.log(`Encoded: ${fontName} (${fontData.length.toLocaleString()} bytes)`);
}

const fontsSection = `[Fonts]\n${fontEntries.join("\n\n")}\n`;

// Insert [Fonts] section before [Events]
let output;
const eventsIdx = assContent.indexOf("[Events]");
if (eventsIdx !== -1) {
  output = assContent.slice(0, eventsIdx) + fontsSection + "\n" + assContent.slice(eventsIdx);
} else {
  output = assContent + "\n" + fontsSection;
}

// Write output
const stem = basename(assPath, ".ass");
const outPath = join(dirname(assPath), `${stem}.embedded.ass`);
writeFileSync(outPath, output, "utf-8");

console.log(`\nOutput: ${outPath}`);
console.log(`Font entries: ${fontEntries.length}`);
console.log(`\nTo verify: play the video with this subtitle in mpv and check if fonts render correctly.`);

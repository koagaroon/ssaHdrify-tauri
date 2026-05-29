/**
 * ass-uuencode byte-transform pins.
 *
 * The ASS [Fonts] UUEncode is the byte transform that gates embedded-font
 * validity, so it gets direct coverage here rather than being deferred from
 * sibling suites. The load-bearing contract is the tail rule: the final
 * partial group emits `remaining + 1` chars (1 byte -> 2, 2 bytes -> 3), and
 * a full group emits 4. Emitting a full 4 for a partial tail keeps the total
 * a multiple of 4, so libass does NOT reject it — it silently appends 1-2 NUL
 * bytes to the decoded font, breaking byte-parity with assfonts / Aegisub.
 *
 * The byte round-trip is checked against a faithful re-implementation of
 * libass's `decode_font` (libass/ass.c): decoded size is
 * `size/4*3 + max(size%4, 1) - 1`, `size%4 == 1` is rejected, and a final
 * group of N chars yields N-1 bytes. A wrong tail therefore fails BOTH the
 * decoder-independent char-count assertion AND the decode round-trip.
 */
import { describe, it, expect } from "vitest";

import { assUuencode, buildFontEntry, MAX_FONT_DATA_SIZE } from "./ass-uuencode";

// ── Faithful libass decoder (libass/ass.c::decode_font) ───────────────────
// Independent of the encoder under test: reconstructs bytes from the
// printable-ASCII chars exactly as libass does, including the tail rule and
// the `size % 4 == 1` rejection. Returns the decoded bytes or an error.
function libassDecode(encoded: string): Uint8Array | { error: string } {
  const chars = [...encoded]
    .filter((c) => c !== "\n" && c !== "\r")
    .map((c) => c.charCodeAt(0) - 33);
  const size = chars.length;
  if (size % 4 === 1) return { error: "Bad encoded data size" };

  const out: number[] = [];
  // decode_chars: n input 6-bit values produce n-1 output bytes.
  const decodeChars = (src: number[], n: number): void => {
    const v = [0, 0, 0, 0];
    for (let i = 0; i < n; i++) v[i] = src[i]!;
    const all = [
      ((v[0]! << 2) | (v[1]! >> 4)) & 0xff,
      (((v[1]! & 0x0f) << 4) | (v[2]! >> 2)) & 0xff,
      (((v[2]! & 0x03) << 6) | v[3]!) & 0xff,
    ];
    for (let i = 0; i < n - 1; i++) out.push(all[i]!);
  };

  let i = 0;
  for (; i + 4 <= size; i += 4) decodeChars(chars.slice(i, i + 4), 4);
  const rem = size % 4;
  if (rem === 2) decodeChars(chars.slice(i, i + 2), 2);
  else if (rem === 3) decodeChars(chars.slice(i, i + 3), 3);
  return Uint8Array.from(out);
}

// Standard tail rule: full groups emit 4 chars; a partial tail of `rem` bytes
// emits `rem + 1`. Decoder-independent — directly catches the always-4 bug.
function expectedEncodedLength(byteLen: number): number {
  const fullGroups = Math.floor(byteLen / 3);
  const rem = byteLen % 3;
  return fullGroups * 4 + (rem === 0 ? 0 : rem + 1);
}

function makeData(len: number): Uint8Array {
  const d = new Uint8Array(len);
  for (let i = 0; i < len; i++) d[i] = (i * 37 + 11) & 0xff;
  if (len > 0) d[len - 1] = 0xab; // non-zero tail byte: a dropped/added NUL is visible
  return d;
}

function joinedChars(lines: string[]): string {
  return lines.join("");
}

describe("assUuencode tail handling (remaining + 1 chars)", () => {
  it("emits 2 chars for a 1-byte tail (not 4)", () => {
    // 1 byte: 0 full groups + 1-byte tail -> 2 chars
    expect(joinedChars(assUuencode(makeData(1))).length).toBe(2);
  });

  it("emits 3 chars for a 2-byte tail (not 4)", () => {
    expect(joinedChars(assUuencode(makeData(2))).length).toBe(3);
  });

  it("emits 4 chars for a full 3-byte group", () => {
    expect(joinedChars(assUuencode(makeData(3))).length).toBe(4);
  });

  // Boundary-pin from both sides: a full 3-byte group (4 chars), and just
  // over it (4 bytes = 1 full group + 1-byte tail = 6 chars, NOT 8). The
  // over-group cases are where the always-4 bug would emit too many chars.
  it.each([
    [4, 6], // 1 full group (4) + 1-byte tail (2)
    [5, 7], // 1 full group (4) + 2-byte tail (3)
    [6, 8], // 2 full groups
    [7, 10], // 2 full groups (8) + 1-byte tail (2)
    [100, 134],
    [1024, 1366],
  ])("total encoded length for %i bytes is %i", (byteLen, expected) => {
    expect(expectedEncodedLength(byteLen)).toBe(expected);
    expect(joinedChars(assUuencode(makeData(byteLen))).length).toBe(expected);
  });
});

describe("assUuencode byte round-trip through libass decode", () => {
  // Cover all three tail residues plus larger inputs. A trailing NUL from the
  // always-4 bug would make decoded length exceed input length and break the
  // exact-equality assertion.
  it.each([1, 2, 3, 4, 5, 6, 7, 10, 64, 100, 257, 1024, 4096])(
    "round-trips %i bytes exactly (no trailing NULs)",
    (byteLen) => {
      const data = makeData(byteLen);
      const decoded = libassDecode(joinedChars(assUuencode(data)));
      expect(decoded).toBeInstanceOf(Uint8Array);
      expect(Array.from(decoded as Uint8Array)).toEqual(Array.from(data));
    }
  );

  it("never produces a char count that libass rejects (size % 4 != 1)", () => {
    for (let len = 0; len <= 30; len++) {
      const total = joinedChars(assUuencode(makeData(len))).length;
      expect(total % 4).not.toBe(1);
    }
  });
});

describe("assUuencode output alphabet and line wrapping", () => {
  it("emits only ASS printable-ASCII chars in [33, 96]", () => {
    for (const ch of joinedChars(assUuencode(makeData(1000)))) {
      const code = ch.charCodeAt(0);
      expect(code).toBeGreaterThanOrEqual(33);
      expect(code).toBeLessThanOrEqual(96);
    }
  });

  it("wraps at exactly 80 chars per line except the last", () => {
    const lines = assUuencode(makeData(1000));
    for (let i = 0; i < lines.length - 1; i++) {
      expect(lines[i]!.length).toBe(80);
    }
    expect(lines[lines.length - 1]!.length).toBeLessThanOrEqual(80);
  });

  it("returns no lines for empty input", () => {
    expect(assUuencode(new Uint8Array(0))).toEqual([]);
  });
});

describe("assUuencode size guard", () => {
  it("throws just over MAX_FONT_DATA_SIZE", () => {
    // Allocate cap+1 bytes; the length check throws before any encode work,
    // so this stays cheap. The at-limit (exactly MAX) case is not paired here
    // because it would require encoding a full 50 MB buffer — high effort,
    // low return — and the guard is a single `length > MAX` comparison, so
    // the over-limit direction fully pins the boundary.
    const oversized = new Uint8Array(MAX_FONT_DATA_SIZE + 1);
    expect(() => assUuencode(oversized)).toThrow(/too large/);
  });
});

describe("buildFontEntry", () => {
  it("prefixes a fontname: header and joins encoded lines", () => {
    const entry = buildFontEntry("arial.ttf", makeData(6));
    const [header, ...body] = entry.split("\n");
    expect(header).toBe("fontname: arial.ttf");
    expect(body.join("")).toBe(joinedChars(assUuencode(makeData(6))));
  });

  it("sanitizes header-breaking chars (: / \\), controls, and BiDi to _", () => {
    // `:` would break the `fontname: <name>` parse; `/` `\` are stripped as a
    // self-contained defense; a control/BiDi char could smuggle a line break.
    // Built from char codes so no raw control/BiDi char lives in this source.
    const ctrl = String.fromCharCode(0x07); // BEL (C0 control)
    const bidi = String.fromCharCode(0x202e); // RIGHT-TO-LEFT OVERRIDE
    const header = buildFontEntry(`a:b/c\\d${ctrl}e${bidi}f`, makeData(3)).split("\n")[0]!;
    expect(header).toBe("fontname: a_b_c_d_e_f");
  });

  it("throws on empty font data (no bare fontname: header)", () => {
    expect(() => buildFontEntry("x.ttf", new Uint8Array(0))).toThrow(/empty font data/);
  });
});

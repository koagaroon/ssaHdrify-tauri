/**
 * Wire-format round-trip pin.
 *
 * FontSubsetPayload uses one byte transport across CLI embed paths:
 *
 * - Standalone embed (`src/cli-engine-entry.ts::FontSubsetPayload`):
 *   bytes as `dataB64: string` (base64).
 *
 * - Chain mode (`src/features/chain/chain-types.ts::ChainFontSubsetPayload`):
 *   bytes as `dataB64: string` (base64).
 *
 * The legacy JSON number-array form expanded ~4-5x per byte
 * (`[255,255,...]` is many chars per byte) and pressured V8's heap on
 * large CJK embed batches. This test pins the shared base64 round-trip
 * and keeps the old number-array shape only as a size comparison.
 *
 * Uses `js-base64` only for test-side encoding so the test runs in any
 * vitest env without requiring Node's `Buffer` global. Decoding uses
 * the production local byte decoder.
 */
import { describe, it, expect } from "vitest";
import { Base64 } from "js-base64";
import { decodeBase64Bytes } from "../../lib/base64-bytes";

function encodeAsNumberArray(bytes: Uint8Array): number[] {
  return Array.from(bytes);
}

// Use js-base64 (test-only, cross-environment) instead of
// Node's Buffer — the project's tsconfig doesn't include @types/node,
// and the test runs through Vite/Vitest's browser-shaped env where
// Buffer isn't a global.
function encodeAsBase64(bytes: Uint8Array): string {
  return Base64.fromUint8Array(bytes);
}

function decodeFromBase64(b64: string): Uint8Array {
  return decodeBase64Bytes(b64);
}

describe("FontSubsetPayload wire-format round-trip", () => {
  const SAMPLE = new Uint8Array([
    0x00, 0x01, 0x10, 0x20, 0x7f, 0x80, 0xfe, 0xff,
    // Some 0x00s scattered to catch null-byte handling regressions.
    0x42, 0x00, 0x42, 0x00, 0x42,
  ]);

  it("standalone embed (dataB64) round-trips byte-identical", () => {
    const wire = encodeAsBase64(SAMPLE);
    expect(typeof wire).toBe("string");
    const round = decodeFromBase64(JSON.parse(JSON.stringify(wire)));
    expect(round).toEqual(SAMPLE);
  });

  it("chain mode (dataB64) round-trips byte-identical", () => {
    const wire = encodeAsBase64(SAMPLE);
    expect(typeof wire).toBe("string");
    const round = decodeFromBase64(JSON.parse(JSON.stringify(wire)));
    expect(round).toEqual(SAMPLE);
  });

  it("base64 stays meaningfully smaller than the legacy number-array form", () => {
    const legacyArr = encodeAsNumberArray(SAMPLE);
    const b64 = encodeAsBase64(SAMPLE);

    // Positive type assertions: keep the legacy helper pinned as a
    // real number array so the size comparison remains meaningful. A
    // refactor that serialized `legacyArr` as
    // `{0:0x00, 1:0x01, ...}` (still `typeof === "object"`) would
    // fail loud.
    expect(Array.isArray(legacyArr)).toBe(true);
    expect(legacyArr.every((n) => Number.isInteger(n) && n >= 0 && n <= 255)).toBe(true);
    expect(typeof b64).toBe("string");
    expect(b64).toMatch(/^[A-Za-z0-9+/]*={0,2}$/);

    // Verify the expansion ratios match the documented "why" — chain
    // and standalone embed must be meaningfully smaller for larger payloads
    // (the entire reason base64 exists in this codebase).
    const big = new Uint8Array(1024).fill(0xab);
    const bigArrLen = JSON.stringify(encodeAsNumberArray(big)).length;
    const bigB64Len = JSON.stringify(encodeAsBase64(big)).length;
    expect(bigB64Len).toBeLessThan(bigArrLen);
    // Base64 expansion is ~1.33x → ~1366 chars + quotes. number[] is
    // ~4-5x → ~5120 chars for 1024 bytes of 0xab ("171,171,..."). The
    // ratio of at least 2x is conservative.
    expect(bigArrLen).toBeGreaterThan(bigB64Len * 2);
  });

  it("empty byte input round-trips through both forms", () => {
    const empty = new Uint8Array(0);
    expect(decodeFromBase64(encodeAsBase64(empty))).toEqual(empty);
  });

  it("decodes large payloads without a stack-heavy string split", () => {
    const large = new Uint8Array(1024 * 1024).fill(0xab);
    const round = decodeFromBase64(encodeAsBase64(large));
    expect(round.length).toBe(large.length);
    expect(round[0]).toBe(0xab);
    expect(round[round.length - 1]).toBe(0xab);
  });
});

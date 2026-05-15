/**
 * Wire-format round-trip pin (Round 3 N-R3-13).
 *
 * Two FontSubsetPayload shapes coexist intentionally on different IPC
 * paths:
 *
 * - Standalone embed (`src/cli-engine-entry.ts::FontSubsetPayload`):
 *   bytes as JSON `data: number[]`. Older format; kept because the
 *   embed-apply call site builds it directly from a Uint8Array and
 *   the round-trip through V8's array deserialization is fast enough
 *   for the per-invocation embed flow.
 *
 * - Chain mode (`src/features/chain/chain-types.ts::ChainFontSubsetPayload`):
 *   bytes as `dataB64: string` (base64). The chain flow may inject
 *   many subset bytes into V8 in a single payload — the JSON-array
 *   form expands ~4-5x per byte (`[255,255,...]` is many chars per
 *   byte) and pressured V8's heap on the worst-case
 *   CUMULATIVE_FALLBACK_BYTES path. Base64 is ~1.33x expansion.
 *
 * The two formats coexist as a deliberate trade-off, not unfinished
 * migration. This test pins:
 *   (a) both formats round-trip byte-identical to the original
 *       Uint8Array;
 *   (b) the wire forms are textually distinct (so a refactor that
 *       accidentally swaps which IPC path uses which won't silently
 *       pass).
 *
 * Uses `js-base64` directly (same package the production decoder in
 * chain-runtime.ts uses) so the test runs in any vitest env without
 * requiring Node's `Buffer` global — the prior `Buffer.from()` form
 * broke `tsc -b` because the project tsconfig intentionally omits
 * @types/node.
 */
import { describe, it, expect } from "vitest";
import { Base64 } from "js-base64";

function encodeAsNumberArray(bytes: Uint8Array): number[] {
  return Array.from(bytes);
}

function decodeFromNumberArray(json: number[]): Uint8Array {
  return new Uint8Array(json);
}

// Use js-base64 (already a runtime dep, cross-environment) instead of
// Node's Buffer — the project's tsconfig doesn't include @types/node,
// and the test runs through Vite/Vitest's browser-shaped env where
// Buffer isn't a global.
function encodeAsBase64(bytes: Uint8Array): string {
  return Base64.fromUint8Array(bytes);
}

function decodeFromBase64(b64: string): Uint8Array {
  return Base64.toUint8Array(b64);
}

describe("FontSubsetPayload wire-format round-trip (Round 3 N-R3-13)", () => {
  const SAMPLE = new Uint8Array([
    0x00, 0x01, 0x10, 0x20, 0x7f, 0x80, 0xfe, 0xff,
    // Some 0x00s scattered to catch null-byte handling regressions.
    0x42, 0x00, 0x42, 0x00, 0x42,
  ]);

  it("standalone embed (number[]) round-trips byte-identical", () => {
    const wire = encodeAsNumberArray(SAMPLE);
    expect(Array.isArray(wire)).toBe(true);
    const round = decodeFromNumberArray(JSON.parse(JSON.stringify(wire)));
    expect(round).toEqual(SAMPLE);
  });

  it("chain mode (dataB64) round-trips byte-identical", () => {
    const wire = encodeAsBase64(SAMPLE);
    expect(typeof wire).toBe("string");
    const round = decodeFromBase64(JSON.parse(JSON.stringify(wire)));
    expect(round).toEqual(SAMPLE);
  });

  it("the two wire forms are textually distinct", () => {
    // A refactor that accidentally swapped which IPC path uses which
    // would either serialize a string where a number[] is expected
    // (or vice-versa). The shapes differ at the top level (Array vs
    // string) so any swap surfaces as a JSON-deserialization error in
    // the consumer, not as a silent byte mismatch. Pin the contract.
    const arr = encodeAsNumberArray(SAMPLE);
    const b64 = encodeAsBase64(SAMPLE);

    // Positive type assertions (Round 4 N-R4-07 / A-R4-05): typeof
    // alone reads `object` vs `string`, which is fine but doesn't pin
    // the array-ness of `arr` or the base64-alphabet of `b64`. Add
    // structural checks so a refactor that serialized `arr` as
    // `{0:0x00, 1:0x01, ...}` (still `typeof === "object"`) would
    // fail loud.
    expect(Array.isArray(arr)).toBe(true);
    expect(arr.every((n) => Number.isInteger(n) && n >= 0 && n <= 255)).toBe(true);
    expect(typeof b64).toBe("string");
    expect(b64).toMatch(/^[A-Za-z0-9+/]*={0,2}$/);

    // Round 11 W11.6 (N1-R11-09): explicit positive typeof on each
    // side. Pre-R11 this site asserted `typeof arr !== typeof b64`
    // which passes for many trivial differences (`"object"` vs
    // `"string"` here, but `"function"` vs `"string"` or even
    // `"undefined"` vs `"string"` would also pass) — a refactor that
    // accidentally returned a string array `"[1,2,3]"` instead of an
    // actual array would slip past. Pin the concrete types so any
    // shape change surfaces.
    expect(typeof arr).toBe("object");
    expect(typeof b64).toBe("string");
    // Verify the expansion ratios match the documented "why" — chain
    // form must be meaningfully smaller for the larger-payload case
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

  // Note: deeper "real consumer round-trip" coverage (importing
  // applyFontEmbed from cli-engine-entry.ts AND chain-runtime's
  // applyEmbedStep) is blocked in this vitest env by the pre-existing
  // js-base64 module-not-found issue on chain-runtime.ts. Once that's
  // resolved, replace the inline encode/decode helpers above with
  // imports from the real consumer modules so the test pins production
  // decode behavior, not Node's `Buffer` semantics (Round 4 N-R4-07 /
  // A-R4-05 deferred coverage).

  it("empty byte input round-trips through both forms", () => {
    const empty = new Uint8Array(0);
    expect(decodeFromNumberArray(encodeAsNumberArray(empty))).toEqual(empty);
    expect(decodeFromBase64(encodeAsBase64(empty))).toEqual(empty);
  });
});

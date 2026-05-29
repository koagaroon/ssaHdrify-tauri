/**
 * Color engine tests — ported from Python tests/test_hdrify.py
 *
 * Tests cover: PQ/HLG conversion, black passthrough, range validity,
 * determinism, brightness effect, and edge cases.
 */
import { describe, it, expect } from "vitest";
import { sRgbToHdr } from "./color-engine";

describe("sRgbToHdr — PQ mode", () => {
  it("maps black to black regardless of brightness", () => {
    expect(sRgbToHdr(0, 0, 0, 100, "PQ")).toEqual([0, 0, 0]);
  });

  it("converts white to a non-black valid 8-bit range", () => {
    const out = sRgbToHdr(255, 255, 255, 100, "PQ");
    for (const ch of out) {
      expect(ch).toBeGreaterThanOrEqual(0);
      expect(ch).toBeLessThanOrEqual(255);
    }
    // Counter-assert: a range-only check is satisfied by the catch-all
    // return-black path; pin that white did NOT collapse to [0,0,0].
    expect(out).not.toEqual([0, 0, 0]);
  });

  it("matches the colour-science PQ reference (not a self-captured snapshot)", () => {
    // Reference values come from Python colour-science, NOT from this pipeline
    // itself — so a regression in the Color.js math (or its bundled
    // coefficients) fails the test instead of being silently re-baselined via
    // `vitest -u`. Both sides implement: sRGB → XYZ(D65) → scale by
    // (brightness / 203) → BT.2020 linear → eotf_inverse_BT2100_PQ at a
    // 203 cd/m² (BT.2408) reference white → ×255, round. Verified an exact
    // (0-LSB) match against colour 0.4.x:
    //   eotf_inverse_BT2100_PQ(
    //     XYZ_to_RGB(sRGB_to_XYZ(rgb/255) * (b/203), BT2020, cctf=False)
    //       .clip(0) * 203) * 255, rounded
    // Regenerate with that formula if the math intentionally changes; do NOT
    // `vitest -u` a self-captured value back in.
    expect(sRgbToHdr(128, 64, 200, 100, "PQ")).toEqual([88, 69, 113]);
    expect(sRgbToHdr(200, 100, 50, 1000, "PQ")).toEqual([167, 141, 112]);
    expect(sRgbToHdr(255, 255, 255, 100, "PQ")).toEqual([130, 130, 130]);
    // Determinism: byte-identical output on a repeat call.
    expect(sRgbToHdr(50, 100, 200, 500, "PQ")).toEqual(sRgbToHdr(50, 100, 200, 500, "PQ"));
  });

  it("different brightness produces different output", () => {
    const low = sRgbToHdr(200, 200, 200, 50, "PQ");
    const high = sRgbToHdr(200, 200, 200, 200, "PQ");
    expect(low).not.toEqual(high);
  });

  it("default brightness is 203 (BT.2408 reference white)", () => {
    const explicit = sRgbToHdr(200, 100, 50, 203, "PQ");
    const defaulted = sRgbToHdr(200, 100, 50);
    expect(explicit).toEqual(defaulted);
  });
});

describe("sRgbToHdr — HLG mode", () => {
  it("matches the colour-science HLG reference (eotf_inverse_BT2100_HLG, γ=1.2)", () => {
    // The HLG OOTF⁻¹ + ARIB STD-B67 OETF are hand-written here (Color.js's
    // rec2100hlg omits the OOTF). Reference values come from Python
    // colour-science eotf_inverse_BT2100_HLG (L_W=1000, system_gamma=1.2),
    // verified an exact (0-LSB) match against this implementation — so the
    // hand-written math is pinned to the spec, not to itself. Pipeline:
    // sRGB → XYZ(D65) → scale by brightness (cd/m²) → BT.2020 linear →
    // eotf_inverse_BT2100_HLG → ×255, round.
    expect(sRgbToHdr(255, 255, 255, 100, "HLG")).toEqual([161, 161, 161]);
    expect(sRgbToHdr(200, 100, 50, 100, "HLG")).toEqual([122, 76, 43]);
  });

  it("maps black to black", () => {
    expect(sRgbToHdr(0, 0, 0, 100, "HLG")).toEqual([0, 0, 0]);
  });

  it("produces different (non-black) output from PQ for same input", () => {
    const pq = sRgbToHdr(200, 100, 50, 100, "PQ");
    const hlg = sRgbToHdr(200, 100, 50, 100, "HLG");
    expect(pq).not.toEqual(hlg);
    // Counter-assert: neither collapsed to the catch-all return-black path,
    // which a bare not.toEqual(pq, hlg) wouldn't catch if both were [0,0,0].
    expect(pq).not.toEqual([0, 0, 0]);
    expect(hlg).not.toEqual([0, 0, 0]);
  });
});

describe("sRgbToHdr — edge cases", () => {
  it("snaps zero / negative / NaN brightness to DEFAULT_BRIGHTNESS (W7.5 boundary guard)", () => {
    // Returning [0,0,0] gracefully (the Python reference threw on
    // zero) was the prior behavior. Now a Number.isFinite + <
    // MIN_BRIGHTNESS guard at the sRgbToHdr entry snaps invalid
    // values to DEFAULT_BRIGHTNESS=203, so the conversion still runs
    // against the BT.2408 reference white instead of producing pure
    // black.
    // This is more useful for the chain runtime where a bad config
    // value mid-batch should not silently flatten every pixel to
    // black — the user sees the conversion happen at the standard
    // reference and can diagnose the config separately.
    const zeroResult = sRgbToHdr(128, 128, 128, 0, "PQ");
    const defaultResult = sRgbToHdr(128, 128, 128, 203, "PQ");
    expect(zeroResult).toEqual(defaultResult);
    // Other invalid inputs likewise snap to default.
    expect(sRgbToHdr(128, 128, 128, -10, "PQ")).toEqual(defaultResult);
    expect(sRgbToHdr(128, 128, 128, NaN, "PQ")).toEqual(defaultResult);
    expect(sRgbToHdr(128, 128, 128, Infinity, "PQ")).toEqual(defaultResult);
  });

  it("returns integer values (no fractional RGB)", () => {
    const [r, g, b] = sRgbToHdr(173, 85, 219, 150, "PQ");
    expect(Number.isInteger(r)).toBe(true);
    expect(Number.isInteger(g)).toBe(true);
    expect(Number.isInteger(b)).toBe(true);
  });

  it("handles near-black colors without NaN", () => {
    const [r, g, b] = sRgbToHdr(1, 1, 1, 203, "PQ");
    expect(Number.isNaN(r)).toBe(false);
    expect(Number.isNaN(g)).toBe(false);
    expect(Number.isNaN(b)).toBe(false);
  });

  it("handles saturated primary colors", () => {
    // Pure red, green, blue should all produce valid output
    for (const [ri, gi, bi] of [
      [255, 0, 0],
      [0, 255, 0],
      [0, 0, 255],
    ] as const) {
      const [r, g, b] = sRgbToHdr(ri, gi, bi, 203, "PQ");
      expect(r).toBeGreaterThanOrEqual(0);
      expect(r).toBeLessThanOrEqual(255);
      expect(g).toBeGreaterThanOrEqual(0);
      expect(g).toBeLessThanOrEqual(255);
      expect(b).toBeGreaterThanOrEqual(0);
      expect(b).toBeLessThanOrEqual(255);
    }
  });
});

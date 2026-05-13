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

  it("converts white to valid 8-bit range", () => {
    const [r, g, b] = sRgbToHdr(255, 255, 255, 100, "PQ");
    for (const ch of [r, g, b]) {
      expect(ch).toBeGreaterThanOrEqual(0);
      expect(ch).toBeLessThanOrEqual(255);
    }
  });

  it("is deterministic — same input always produces same output", () => {
    const a = sRgbToHdr(128, 64, 200, 100, "PQ");
    const b = sRgbToHdr(128, 64, 200, 100, "PQ");
    expect(a).toEqual(b);
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
  it("converts white to valid 8-bit range", () => {
    const [r, g, b] = sRgbToHdr(255, 255, 255, 100, "HLG");
    for (const ch of [r, g, b]) {
      expect(ch).toBeGreaterThanOrEqual(0);
      expect(ch).toBeLessThanOrEqual(255);
    }
  });

  it("maps black to black", () => {
    expect(sRgbToHdr(0, 0, 0, 100, "HLG")).toEqual([0, 0, 0]);
  });

  it("produces different output from PQ for same input", () => {
    const pq = sRgbToHdr(200, 100, 50, 100, "PQ");
    const hlg = sRgbToHdr(200, 100, 50, 100, "HLG");
    expect(pq).not.toEqual(hlg);
  });
});

describe("sRgbToHdr — edge cases", () => {
  it("snaps zero / negative / NaN brightness to DEFAULT_BRIGHTNESS (W7.5 boundary guard)", () => {
    // Pre-W7.5 returned [0,0,0] graceful (Python reference threw on
    // zero). W7.5 introduces a Number.isFinite + < MIN_BRIGHTNESS
    // guard at the sRgbToHdr entry that snaps invalid values to
    // DEFAULT_BRIGHTNESS=203, so the conversion still runs against
    // the BT.2408 reference white instead of producing pure black.
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

/**
 * HDR color conversion engine — port of Python hdrify.py sRgbToHdr().
 *
 * PQ pipeline: sRGB → Color.js xyz-d65 → luminance scale → rec2100pq → 0-255
 *   Uses Color.js end-to-end. Verified 8/8 exact match with Python.
 *
 * HLG pipeline: sRGB → Color.js xyz-d65 → rec2020-linear → manual OOTF⁻¹ + OETF → 0-255
 *   Color.js only handles sRGB→XYZ→BT.2020 matrix math. The HLG encoding
 *   (inverse OOTF + ARIB STD-B67 OETF) is implemented manually to match
 *   Python colour-science's eotf_inverse_BT2100_HLG with system_gamma=1.2.
 *   Color.js's built-in rec2100hlg is NOT used because it omits the OOTF entirely.
 */
import Color from "colorjs.io";

export type Eotf = "PQ" | "HLG";

/** BT.2408 reference white luminance (nits) */
export const DEFAULT_BRIGHTNESS = 203;
export const MIN_BRIGHTNESS = 1;
export const MAX_BRIGHTNESS = 10000;

// ── HLG constants (BT.2100-2 / ARIB STD-B67) ───────────

/** Nominal peak display luminance (cd/m²) */
const HLG_L_W = 1000;

/** System gamma for L_W = 1000 cd/m²: γ = 1.2 + 0.42 * log10(L_W / 1000) */
const HLG_GAMMA = 1.2;

/** Rec.2020 luminance coefficients (BT.2020 / BT.2100) */
const REC2020_LUM_R = 0.2627;
const REC2020_LUM_G = 0.678;
const REC2020_LUM_B = 0.0593;

/** ARIB STD-B67 OETF constants */
const HLG_A = 0.17883277;
const HLG_B = 0.28466892; // 1 - 4a
const HLG_C = 0.55991073; // 0.5 - a * ln(4a)

/**
 * HLG OETF (ARIB STD-B67): scene-linear [0,1] → HLG signal [0,1]
 */
function hlgOetf(E: number): number {
  if (E <= 1 / 12) return Math.sqrt(3 * E);
  return HLG_A * Math.log(12 * E - HLG_B) + HLG_C;
}

/**
 * Convert sRGB to HLG using the full BT.2100 inverse EOTF path:
 * sRGB → XYZ → BT.2020 linear (absolute cd/m²) → inverse OOTF → OETF → signal
 *
 * This matches Python's colour-science eotf_inverse_BT2100_HLG(system_gamma=1.2).
 */
function sRgbToHlg(
  r: number,
  g: number,
  b: number,
  targetBrightness: number
): [number, number, number] {
  // 1. sRGB → xyz-d65 (Color.js, relative luminance: Y=1.0 at D65 white)
  const srgb = new Color("srgb", [r / 255, g / 255, b / 255]);
  const xyz = srgb.to("xyz-d65");

  const y = xyz.coords[1] ?? 0;
  if (y <= 0) return [0, 0, 0];

  // 2. Scale to absolute display luminance (cd/m²).
  // Relative Y=1.0 → absolute = targetBrightness cd/m².
  // This matches Python: xyY_hdr_color[2] = xyY_sdr_color[2] * srgb_brightness
  // Use a new Color object instead of mutating xyz.coords in place — avoids
  // relying on undocumented Color.js mutation-before-to() behavior.
  const scaledXyz = new Color("xyz-d65", [
    (xyz.coords[0] ?? 0) * targetBrightness,
    (xyz.coords[1] ?? 0) * targetBrightness,
    (xyz.coords[2] ?? 0) * targetBrightness,
  ]);

  // 3. XYZ → BT.2020 linear RGB (absolute cd/m²)
  // Use Color.js for the matrix conversion only
  const bt2020 = scaledXyz.to("rec2020-linear");
  const R_D = bt2020.coords[0] ?? 0;
  const G_D = bt2020.coords[1] ?? 0;
  const B_D = bt2020.coords[2] ?? 0;

  // 4. Inverse OOTF: display-referred (cd/m²) → scene-referred (normalized 0–1)
  //
  // BT.2100-2 OOTF: E_D = L_W * E_S * Y_S^(γ-1)
  // Inverse: E_S = E_D * Y_D^((1-γ)/γ) / L_W^(1/γ)
  //   where Y_D = 0.2627*R_D + 0.6780*G_D + 0.0593*B_D
  const Y_D = REC2020_LUM_R * R_D + REC2020_LUM_G * G_D + REC2020_LUM_B * B_D;
  if (Y_D <= 0) return [0, 0, 0];

  const factor = Math.pow(Y_D, (1 - HLG_GAMMA) / HLG_GAMMA) / Math.pow(HLG_L_W, 1 / HLG_GAMMA);
  const R_S = R_D * factor;
  const G_S = G_D * factor;
  const B_S = B_D * factor;

  // 5. Apply HLG OETF: scene-linear → HLG signal [0,1]
  const R_hlg = hlgOetf(Math.max(0, R_S));
  const G_hlg = hlgOetf(Math.max(0, G_S));
  const B_hlg = hlgOetf(Math.max(0, B_S));

  // 6. Scale to 0-255, clip, round
  return [
    Math.round(Math.max(0, Math.min(255, R_hlg * 255))),
    Math.round(Math.max(0, Math.min(255, G_hlg * 255))),
    Math.round(Math.max(0, Math.min(255, B_hlg * 255))),
  ];
}

/**
 * Convert a single sRGB color to HDR (BT.2100 PQ or HLG).
 *
 * @param r - Red channel 0-255
 * @param g - Green channel 0-255
 * @param b - Blue channel 0-255
 * @param targetBrightness - Absolute luminance in nits (default 203, BT.2408)
 * @param eotf - Transfer function: "PQ" or "HLG"
 * @returns [R, G, B] integers in 0-255 range (rounded via Math.round)
 */
export function sRgbToHdr(
  r: number,
  g: number,
  b: number,
  targetBrightness: number = DEFAULT_BRIGHTNESS,
  eotf: Eotf = "PQ"
): [number, number, number] {
  // Black passthrough — no conversion needed
  if (r === 0 && g === 0 && b === 0) {
    return [0, 0, 0];
  }

  // Round 7 Wave 7.5 (A4-R7-1): guard against NaN / Infinity / sub-1
  // targetBrightness. NaN propagates through all subsequent math
  // (scale = NaN / 203 = NaN, every pixel → 0 or undefined behavior).
  // Negative or zero produces nonsense output without erroring out.
  // Sub-1 is rejected because PQ + HLG transfer functions assume
  // ≥1 nit reference; out of that range the math still runs but the
  // result is meaningless. Snap silently to DEFAULT_BRIGHTNESS rather
  // than throwing — the conversion pipeline runs against many
  // user-influenced inputs and a hard throw here would abort entire
  // subtitle conversion for one bad config value.
  if (!Number.isFinite(targetBrightness) || targetBrightness < MIN_BRIGHTNESS) {
    targetBrightness = DEFAULT_BRIGHTNESS;
  }

  try {
    // HLG uses a dedicated path with manual OOTF + OETF
    if (eotf === "HLG") {
      return sRgbToHlg(r, g, b, targetBrightness);
    }

    // PQ path: use Color.js end-to-end
    const srgb = new Color("srgb", [r / 255, g / 255, b / 255]);
    const xyz = srgb.to("xyz-d65");

    const y = xyz.coords[1] ?? 0;
    if (y <= 0) return [0, 0, 0];

    // Scale luminance by target brightness.
    // Color.js xyz-d65 Y is relative (1.0 = D65 white).
    // PQ's reference white is DEFAULT_BRIGHTNESS nits per BT.2408.
    // Use a new Color object instead of mutating in place — avoids relying
    // on undocumented Color.js mutation-before-to() behavior.
    const scale = targetBrightness / DEFAULT_BRIGHTNESS;
    const scaledXyz = new Color("xyz-d65", [
      (xyz.coords[0] ?? 0) * scale,
      (xyz.coords[1] ?? 0) * scale,
      (xyz.coords[2] ?? 0) * scale,
    ]);

    const hdr = scaledXyz.to("rec2100pq");

    const result = hdr.coords.map((c: number | null) => {
      const v = c ?? 0;
      if (Number.isNaN(v)) return 0;
      return Math.round(Math.max(0, Math.min(255, v * 255)));
    });

    return [result[0], result[1], result[2]];
  } catch (e) {
    // Any conversion error → black. The conversion always returns a
    // valid [0,0,0] triple — operation succeeds for the caller — so
    // per the log-discipline rule (~/.claude/rules/vibe-coding.md
    // "log-level discipline"), the success-of-degradation site uses
    // DEBUG, not WARN. Stays visible with `localStorage.debug = '*'`
    // or DevTools verbose-logging when actually investigating
    // malformed-color edge cases. Round 1 F2.N-R1-15.
    console.debug(`[ssaHdrify] sRgbToHdr failed for (${r},${g},${b}):`, e);
    return [0, 0, 0];
  }
}

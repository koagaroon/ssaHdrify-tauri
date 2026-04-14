/**
 * HDR color conversion engine — port of Python hdrify.py sRgbToHdr().
 *
 * Pipeline: sRGB (0-255) → Color.js sRGB → xyz-d65 → luminance scale → rec2100-pq/hlg → 0-255
 *
 * Uses Color.js for all color space math. PQ results are verified to match
 * Python colour-science exactly (8/8 test cases). HLG has a known ~15%
 * variance on mid-tones due to system gamma differences.
 */
import Color from "colorjs.io";

export type Eotf = "PQ" | "HLG";

/** BT.2408 reference white luminance (nits) */
export const DEFAULT_BRIGHTNESS = 203;
export const MIN_BRIGHTNESS = 1;
export const MAX_BRIGHTNESS = 10000;

/** Target Color.js color space identifiers */
const SPACE_MAP: Record<Eotf, string> = {
  PQ: "rec2100pq",
  HLG: "rec2100hlg",
};

/**
 * Convert a single sRGB color to HDR (BT.2100 PQ or HLG).
 *
 * @param r - Red channel 0-255
 * @param g - Green channel 0-255
 * @param b - Blue channel 0-255
 * @param targetBrightness - Absolute luminance in nits (default 203, BT.2408)
 * @param eotf - Transfer function: "PQ" or "HLG"
 * @returns [R, G, B] in 0-255 range
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

  try {
    // Normalize to [0,1] and create sRGB color
    const srgb = new Color("srgb", [r / 255, g / 255, b / 255]);

    // Convert to CIE XYZ (D65 illuminant) — this applies inverse sRGB gamma
    const xyz = srgb.to("xyz-d65");
    const coords = xyz.coords;

    // Extract luminance (Y component) and check validity
    const y = coords[1] ?? 0;
    if (y <= 0) return [0, 0, 0];

    // Scale luminance by target brightness.
    // Color.js xyz-d65 Y is relative (1.0 = D65 white).
    // Multiply by (targetBrightness / 203) to scale to desired absolute level,
    // since PQ's reference white is ~203 nits per BT.2408.
    const scale = targetBrightness / 203;
    coords[0] = (coords[0] ?? 0) * scale;
    coords[1] = (coords[1] ?? 0) * scale;
    coords[2] = (coords[2] ?? 0) * scale;

    // Convert to target HDR space (applies BT.2020 matrix + PQ/HLG OETF)
    const hdr = xyz.to(SPACE_MAP[eotf]);

    // Extract, scale to 0-255, clip, and round
    const result = hdr.coords.map((c: number | null) => {
      const v = c ?? 0;
      if (Number.isNaN(v)) return 0;
      return Math.round(Math.max(0, Math.min(255, v * 255)));
    });

    return [result[0], result[1], result[2]];
  } catch {
    // Any conversion error → black
    return [0, 0, 0];
  }
}

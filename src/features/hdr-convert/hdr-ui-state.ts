import { parseFiniteNumberText } from "../../lib/strict-number";

export interface HdrConvertDisabledState {
  hasFiles: boolean;
  processing: boolean;
  brightnessInvalid: boolean;
}

export function isHdrBrightnessInvalid(
  brightnessText: string,
  minBrightness: number,
  maxBrightness: number
): boolean {
  const value = parseFiniteNumberText(brightnessText);
  return value === null || value < minBrightness || value > maxBrightness;
}

export function isHdrConvertDisabled(state: HdrConvertDisabledState): boolean {
  return !state.hasFiles || state.processing || state.brightnessInvalid;
}

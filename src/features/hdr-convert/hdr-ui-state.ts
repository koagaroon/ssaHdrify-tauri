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
  const text = brightnessText.trim();
  if (text === "") return true;
  const value = parseFloat(text);
  return !Number.isFinite(value) || value < minBrightness || value > maxBrightness;
}

export function isHdrConvertDisabled(state: HdrConvertDisabledState): boolean {
  return !state.hasFiles || state.processing || state.brightnessInvalid;
}

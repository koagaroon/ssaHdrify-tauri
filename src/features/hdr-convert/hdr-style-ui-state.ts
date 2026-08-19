import { parseFiniteNumberText } from "../../lib/strict-number";
import { isValidMicroDvdFps } from "../../lib/subtitle-parser";

export type HdrFpsMode = "auto" | "manual";

export function parseHdrStyleNumberInput(
  text: string,
  minValue: number,
  maxValue: number
): number | null {
  const value = parseFiniteNumberText(text);
  if (value === null || value < minValue || value > maxValue) return null;
  return value;
}

/** Auto is represented by `undefined`; `null` means invalid manual text. */
export function parseHdrFpsOverride(mode: HdrFpsMode, text: string): number | undefined | null {
  if (mode === "auto") return undefined;
  const value = parseFiniteNumberText(text);
  if (value === null || !isValidMicroDvdFps(value)) return null;
  return value;
}

export function isHdrFpsInvalid(mode: HdrFpsMode, text: string): boolean {
  return parseHdrFpsOverride(mode, text) === null;
}

export function isMicroDvdInputName(fileName: string): boolean {
  return fileName.toLowerCase().endsWith(".sub");
}

export function hasMicroDvdInput(fileNames: readonly string[]): boolean {
  return fileNames.some(isMicroDvdInputName);
}

export function parseHdrFpsOverrideForInput(
  fileName: string,
  mode: HdrFpsMode,
  text: string
): number | undefined | null {
  return isMicroDvdInputName(fileName) ? parseHdrFpsOverride(mode, text) : undefined;
}

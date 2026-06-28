import { parseFiniteNumberText } from "../../lib/strict-number";

export function parseHdrStyleNumberInput(
  text: string,
  minValue: number,
  maxValue: number
): number | null {
  const value = parseFiniteNumberText(text);
  if (value === null || value < minValue || value > maxValue) return null;
  return value;
}

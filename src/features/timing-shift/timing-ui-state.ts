export interface TimingSaveDisabledState {
  fileCount: number;
  thresholdInvalid: boolean;
  offsetInvalid: boolean;
  busy: boolean;
}

export type TimingOffsetUnit = "ms" | "s";

/** Parse a nonnegative displayed magnitude into exact integer milliseconds.
 * The direction control owns the sign. Millisecond input is integer-only;
 * seconds can use at most three decimals, which maps exactly to milliseconds. */
export function parseTimingOffsetMagnitudeMs(
  offsetText: string,
  unit: TimingOffsetUnit,
  offsetMax: number
): number | null {
  if (offsetText !== offsetText.trim() || offsetText.length === 0) return null;
  const pattern = unit === "ms" ? /^\d+$/ : /^(?:\d+(?:\.\d{0,3})?|\.\d{1,3})$/;
  if (!pattern.test(offsetText)) return null;

  const value = Number(offsetText);
  if (!Number.isFinite(value) || value < 0 || value > offsetMax) return null;
  if (unit === "ms") return Number.isSafeInteger(value) ? value : null;

  const [wholeText, fractionText = ""] = offsetText.split(".");
  const wholeSeconds = Number(wholeText || "0");
  const fractionalMs = Number(fractionText.padEnd(3, "0") || "0");
  const milliseconds = wholeSeconds * 1000 + fractionalMs;
  return Number.isSafeInteger(milliseconds) ? milliseconds : null;
}

export function formatTimingOffsetMagnitude(magnitudeMs: number, unit: TimingOffsetUnit): string {
  if (unit === "ms") return magnitudeMs.toString();
  const wholeSeconds = Math.floor(magnitudeMs / 1000);
  const fraction = String(magnitudeMs % 1000)
    .padStart(3, "0")
    .replace(/0+$/, "");
  return fraction ? `${wholeSeconds}.${fraction}` : wholeSeconds.toString();
}

export function isTimingOffsetInvalid(
  offsetText: string,
  unit: TimingOffsetUnit,
  offsetMax: number
): boolean {
  return parseTimingOffsetMagnitudeMs(offsetText, unit, offsetMax) === null;
}

export function isTimingSaveDisabled(state: TimingSaveDisabledState): boolean {
  return state.fileCount === 0 || state.thresholdInvalid || state.offsetInvalid || state.busy;
}

import { parseFiniteNumberText } from "../../lib/strict-number";

export interface TimingSaveDisabledState {
  fileCount: number;
  thresholdInvalid: boolean;
  offsetInvalid: boolean;
  busy: boolean;
}

export function isTimingOffsetInvalid(offsetText: string, offsetMax: number): boolean {
  const value = parseFiniteNumberText(offsetText);
  return value === null || Math.abs(value) > offsetMax;
}

export function isTimingSaveDisabled(state: TimingSaveDisabledState): boolean {
  return state.fileCount === 0 || state.thresholdInvalid || state.offsetInvalid || state.busy;
}

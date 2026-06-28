export interface TimingSaveDisabledState {
  fileCount: number;
  thresholdInvalid: boolean;
  offsetInvalid: boolean;
  busy: boolean;
}

export function isTimingOffsetInvalid(offsetText: string, offsetMax: number): boolean {
  const text = offsetText.trim();
  if (text === "") return true;
  const value = parseFloat(text);
  return !Number.isFinite(value) || Math.abs(value) > offsetMax;
}

export function isTimingSaveDisabled(state: TimingSaveDisabledState): boolean {
  return state.fileCount === 0 || state.thresholdInvalid || state.offsetInvalid || state.busy;
}

import type { FontCacheStatus } from "./tauri-api";

interface MutableBooleanRef {
  current: boolean;
}

/**
 * Mark a launch-time cache probe complete only when its effect instance is
 * still active. React StrictMode cleans up the first development-only setup
 * before running the second; a cancelled first setup must not latch the probe
 * as complete and suppress that live retry.
 */
export function completeFontCacheProbe(cacheChecked: MutableBooleanRef, cancelled: boolean): void {
  if (!cancelled) cacheChecked.current = true;
}

/** Schema mismatch has its own recovery modal, so it is not also an
 * "unavailable" banner state. A rejected probe is unavailable even though no
 * FontCacheStatus value was returned. */
export function isFontCacheUnavailable(
  status: FontCacheStatus | null,
  probeFailed: boolean
): boolean {
  return probeFailed || (status !== null && !status.available && !status.schemaMismatch);
}

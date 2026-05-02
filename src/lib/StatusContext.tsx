/**
 * StatusContext — per-tab workflow indicator state.
 *
 * Each feature tab (HDR / Timing / Fonts) publishes its own workflow
 * state (idle, pending, busy, done, error) + a short message. The App
 * footer reads the *active* tab's status and renders a colored dot +
 * text so users can tell at a glance whether they need to act, whether
 * something's in progress, or whether the last action succeeded.
 *
 * Context + hook live here. The Provider component lives in
 * StatusProvider.tsx so React Fast Refresh can hot-reload components
 * without a full page reset.
 */
import { createContext, useContext } from "react";

export type StatusKind = "idle" | "pending" | "busy" | "done" | "error";

/** N-of-M progress for batch operations. The footer renders this alongside
 *  `message` when present; consumers pass `undefined` for non-batch states.
 *  Both numbers are required when supplied — partial progress is undefined. */
export interface StatusProgress {
  processed: number;
  total: number;
}

export interface Status {
  kind: StatusKind;
  message: string;
  /** Optional N-of-M progress for batch flows. Omit for single-file or
   *  non-progress states. The footer hides the indicator when this is
   *  undefined or when `total === 0`. */
  progress?: StatusProgress;
}

export type StatusTab = "hdr" | "timing" | "fonts" | "rename";

export interface StatusContextValue {
  statuses: Record<StatusTab, Status>;
  setStatus: (tab: StatusTab, status: Status) => void;
}

// Frozen so an accidental in-place mutation anywhere fails loudly instead
// of silently corrupting every tab's default via the shared object alias.
export const DEFAULT_STATUS: Status = Object.freeze({ kind: "idle", message: "" }) as Status;

// Per-tab literals — frozen individually so in-place mutation of any slot
// fails loudly in dev instead of silently drifting that tab's default.
export const DEFAULT_STATUSES: Record<StatusTab, Status> = Object.freeze({
  hdr: Object.freeze({ ...DEFAULT_STATUS }),
  timing: Object.freeze({ ...DEFAULT_STATUS }),
  fonts: Object.freeze({ ...DEFAULT_STATUS }),
  rename: Object.freeze({ ...DEFAULT_STATUS }),
}) as Record<StatusTab, Status>;

export const StatusContext = createContext<StatusContextValue>({
  statuses: DEFAULT_STATUSES,
  setStatus: () => {},
});

export function useStatus() {
  return useContext(StatusContext);
}

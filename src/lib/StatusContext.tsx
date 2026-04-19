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

export interface Status {
  kind: StatusKind;
  message: string;
}

export type StatusTab = "hdr" | "timing" | "fonts";

export interface StatusContextValue {
  statuses: Record<StatusTab, Status>;
  setStatus: (tab: StatusTab, status: Status) => void;
}

export const DEFAULT_STATUS: Status = { kind: "idle", message: "" };

export const DEFAULT_STATUSES: Record<StatusTab, Status> = {
  hdr: DEFAULT_STATUS,
  timing: DEFAULT_STATUS,
  fonts: DEFAULT_STATUS,
};

export const StatusContext = createContext<StatusContextValue>({
  statuses: DEFAULT_STATUSES,
  setStatus: () => {},
});

export function useStatus() {
  return useContext(StatusContext);
}

/**
 * StatusProvider — holds the per-tab status map and exposes a guarded
 * setter. The setter skips no-op updates so feature components can call
 * it unconditionally inside useEffect without triggering render loops.
 *
 * See StatusContext.tsx for the context / hook / type definitions.
 */
import { useCallback, useMemo, useState, type ReactNode } from "react";
import {
  StatusContext,
  DEFAULT_STATUSES,
  type Status,
  type StatusTab,
} from "./StatusContext";

export default function StatusProvider({ children }: { children: ReactNode }) {
  const [statuses, setStatuses] = useState<Record<StatusTab, Status>>(DEFAULT_STATUSES);

  const setStatus = useCallback((tab: StatusTab, status: Status) => {
    setStatuses((prev) => {
      const current = prev[tab];
      if (current.kind === status.kind && current.message === status.message) {
        return prev;
      }
      return { ...prev, [tab]: status };
    });
  }, []);

  // useMemo keeps the context value referentially stable when neither
  // `statuses` nor `setStatus` changed. Without the memo, every render
  // creates a fresh `{statuses, setStatus}` object → every useStatus()
  // consumer re-renders.
  const value = useMemo(() => ({ statuses, setStatus }), [statuses, setStatus]);

  return <StatusContext.Provider value={value}>{children}</StatusContext.Provider>;
}

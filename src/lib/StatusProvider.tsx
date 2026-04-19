/**
 * StatusProvider — holds the per-tab status map and exposes a guarded
 * setter. The setter skips no-op updates so feature components can call
 * it unconditionally inside useEffect without triggering render loops.
 *
 * See StatusContext.ts for the context / hook / type definitions.
 */
import { useCallback, useState, type ReactNode } from "react";
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

  return (
    <StatusContext.Provider value={{ statuses, setStatus }}>{children}</StatusContext.Provider>
  );
}

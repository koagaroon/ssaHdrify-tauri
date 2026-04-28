/**
 * useLogPanel — shared log buffer + auto-scroll wiring used by every
 * batch tab (HDR Convert / Time Shift / Font Embed / Batch Rename).
 *
 * Each tab originally carried an identical `logs` state, `logIdRef`
 * counter, 200-entry cap, and `setTimeout(scrollTop = scrollHeight, 50)`
 * follow-the-tail effect. Only the entry types and the call sites
 * differed — the wiring was duplicated four times.
 *
 * Why scrollTop directly instead of `scrollIntoView`: the latter walks
 * up the ancestor chain and scrolls every scrollable container. In
 * Chromium it scrolls the `.window` element past the titlebar / header
 * / file strip during batch runs even though `.window` has
 * `overflow: hidden`, because programmatic scrolls bypass the
 * user-driven block. Setting scrollTop on the inner container only is
 * the safe fix.
 */
import { useCallback, useRef, useState } from "react";

export type LogType = "info" | "error" | "success";

export interface LogEntry {
  id: number;
  text: string;
  type: LogType;
}

export interface UseLogPanelResult {
  logs: LogEntry[];
  addLog: (text: string, type?: LogType) => void;
  clearLogs: () => void;
  /** Attach to the inner overflow-y-auto element of the log panel.
   *  Required for auto-scroll-to-tail to work. */
  logScrollRef: React.RefObject<HTMLDivElement | null>;
}

/** Maximum entries kept in the log buffer. Older entries are trimmed
 *  off the head so the array stays bounded during long batch runs. */
const MAX_LOG_ENTRIES = 200;

export function useLogPanel(): UseLogPanelResult {
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const logIdRef = useRef(0);
  const logScrollRef = useRef<HTMLDivElement | null>(null);

  const addLog = useCallback((text: string, type: LogType = "info") => {
    const id = logIdRef.current++;
    setLogs((prev) => {
      const next = [...prev, { id, text, type }];
      return next.length > MAX_LOG_ENTRIES ? next.slice(-MAX_LOG_ENTRIES) : next;
    });
    // Defer the scroll past the React commit so the new row is in the
    // DOM before we read scrollHeight. 50ms matches the original sites.
    setTimeout(() => {
      const el = logScrollRef.current;
      if (el) el.scrollTop = el.scrollHeight;
    }, 50);
  }, []);

  const clearLogs = useCallback(() => setLogs([]), []);

  return { logs, addLog, clearLogs, logScrollRef };
}

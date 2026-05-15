/**
 * useLogPanel — shared log buffer + auto-scroll wiring used by every
 * batch tab (HDR Convert / Time Shift / Font Embed / Batch Rename).
 *
 * Each tab originally carried an identical `logs` state, `logIdRef`
 * counter, 200-entry cap, and follow-the-tail effect. Only the entry
 * types and the call sites differed — the wiring was duplicated four
 * times.
 *
 * Why scrollTop directly instead of `scrollIntoView`: the latter walks
 * up the ancestor chain and scrolls every scrollable container. In
 * Chromium it scrolls the `.window` element past the titlebar / header
 * / file strip during batch runs even though `.window` has
 * `overflow: hidden`, because programmatic scrolls bypass the
 * user-driven block. Setting scrollTop on the inner container only is
 * the safe fix.
 */
import { useCallback, useEffect, useRef, useState } from "react";

export type LogType = "info" | "warn" | "error" | "success";

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
const TAIL_THRESHOLD_PX = 16;
const SCROLL_DELAY_MS = 50;

export function useLogPanel(): UseLogPanelResult {
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const logIdRef = useRef(0);
  const logScrollRef = useRef<HTMLDivElement | null>(null);
  const pendingTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const isNearTail = useCallback((): boolean => {
    const el = logScrollRef.current;
    // W6.7 Round 6 — WHY null-element returns true (not false): the
    // panel hasn't mounted yet, OR it mounted then unmounted without
    // the ref re-resolving. In both cases the safe default is "the
    // next log entry can scroll into view", because the alternative
    // (false) would never trigger `scheduleTailScroll`, and a panel
    // that re-mounts after the first log line would never auto-scroll
    // to the latest entry. False here would also break addLog's
    // "follow-tail" contract on the initial frame, where the ref
    // briefly resolves AFTER the first setLogs call. Tail-following
    // is the user-visible expectation; returning true favors that.
    if (!el) return true;
    return el.scrollHeight - el.scrollTop - el.clientHeight < TAIL_THRESHOLD_PX;
  }, []);

  const scheduleTailScroll = useCallback(() => {
    if (pendingTimerRef.current !== null) {
      clearTimeout(pendingTimerRef.current);
    }
    pendingTimerRef.current = setTimeout(() => {
      pendingTimerRef.current = null;
      const el = logScrollRef.current;
      if (!el) return;
      el.scrollTop = el.scrollHeight;
    }, SCROLL_DELAY_MS);
  }, []);

  const addLog = useCallback(
    (text: string, type: LogType = "info") => {
      const id = logIdRef.current++;
      const shouldFollowTail = isNearTail();
      setLogs((prev) => {
        const next = [...prev, { id, text, type }];
        return next.length > MAX_LOG_ENTRIES ? next.slice(-MAX_LOG_ENTRIES) : next;
      });
      // Defer the scroll past the React commit so the new row is in the DOM
      // before we read scrollHeight. Only follow when the user was already
      // near the tail; if they scrolled up to inspect earlier logs, leave
      // their viewport alone.
      if (shouldFollowTail) scheduleTailScroll();
    },
    [isNearTail, scheduleTailScroll]
  );

  const clearLogs = useCallback(() => setLogs([]), []);

  useEffect(() => {
    return () => {
      if (pendingTimerRef.current !== null) {
        clearTimeout(pendingTimerRef.current);
      }
    };
  }, []);

  return { logs, addLog, clearLogs, logScrollRef };
}

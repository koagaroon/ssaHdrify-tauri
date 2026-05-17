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
/**
 * Per-entry text length cap. Round 11 W11.3 (A3-R11-01): pairs with
 * MAX_LOG_ENTRIES. Without this, attacker-influenced inputs (P1b font /
 * filename / error-message content) could push individual entries to
 * arbitrary length — a 32 KB filename per row × 200 rows ≈ 6 MB
 * retained in React state with the log panel rendering each row's
 * full text. 4 KB / entry is generous (typical messages are 30-200
 * chars; even a verbose multi-path stderr is well under 1 KB).
 * Truncated entries get an ellipsis so the truncation is visible.
 */
const MAX_LOG_ENTRY_TEXT_LEN = 4_096;
const TAIL_THRESHOLD_PX = 16;
const SCROLL_DELAY_MS = 50;

export function useLogPanel(): UseLogPanelResult {
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const logIdRef = useRef(0);
  const logScrollRef = useRef<HTMLDivElement | null>(null);
  const pendingTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const isNearTail = useCallback((): boolean => {
    const el = logScrollRef.current;
    // W6.7 Round 6 — WHY null-element returns true (not false):
    // protects the initial-render race where the ref briefly resolves
    // AFTER the first setLogs call. False here would skip
    // `scheduleTailScroll` for that first log line, breaking the
    // "follow-tail" contract on the very first user-visible entry.
    // (R15 W15.7 N-R15-27: prior comment also claimed a re-mount
    // scenario where false would permanently break auto-scroll;
    // that's overstated — once the panel mounts, the ref non-null
    // settles and the next addLog reads non-null. The initial-frame
    // race is the only real concern.)
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
      // Round 11 W11.3 (A3-R11-01): truncate over-long entries with an
      // ellipsis so MAX_LOG_ENTRIES × per-entry length stays bounded.
      // See MAX_LOG_ENTRY_TEXT_LEN docblock.
      const safeText =
        text.length > MAX_LOG_ENTRY_TEXT_LEN
          ? text.slice(0, MAX_LOG_ENTRY_TEXT_LEN - 1) + "…"
          : text;
      setLogs((prev) => {
        const next = [...prev, { id, text: safeText, type }];
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

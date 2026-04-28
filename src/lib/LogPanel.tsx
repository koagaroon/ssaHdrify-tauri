/**
 * LogPanel — shared log section UI used by every batch tab.
 *
 * Renders the standard "Log" header (title + Clear button) plus the
 * scroll body containing color-coded entries (error / success / info).
 * The four feature tabs originally inlined the same JSX block
 * verbatim; the only variation was the entries themselves and the
 * scroll-ref binding.
 *
 * Pair with `useLogPanel` from `useLogPanel.ts` — that hook owns the
 * log buffer state, addLog callback, and the scrollRef this component
 * binds. The rendered tree is a no-op when `logs` is empty so callers
 * can drop it in unconditionally and let it self-hide.
 */
import type { RefObject } from "react";
import { useI18n } from "../i18n/useI18n";
import type { LogEntry } from "./useLogPanel";

interface LogPanelProps {
  logs: LogEntry[];
  onClear: () => void;
  /** Attach the scroll ref returned by useLogPanel — wires up auto-scroll. */
  scrollRef: RefObject<HTMLDivElement | null>;
}

const LOG_COLOR: Record<LogEntry["type"], string> = {
  error: "var(--error)",
  success: "var(--success)",
  info: "var(--text-muted)",
};

export function LogPanel({ logs, onClear, scrollRef }: LogPanelProps) {
  const { t } = useI18n();
  if (logs.length === 0) return null;

  return (
    <div
      className="rounded-lg"
      style={{ border: "1px solid var(--border)", background: "var(--bg-panel)" }}
    >
      <div
        className="flex items-center justify-between px-3 py-2"
        style={{ borderBottom: "1px solid var(--border)" }}
      >
        <span className="text-xs font-medium" style={{ color: "var(--text-muted)" }}>
          {t("log_title")}
        </span>
        <button onClick={onClear} className="text-xs" style={{ color: "var(--text-muted)" }}>
          {t("log_clear")}
        </button>
      </div>
      <div ref={scrollRef} className="max-h-48 overflow-y-auto p-3 font-mono text-xs space-y-0.5">
        {logs.map((log) => (
          <div key={log.id} style={{ color: LOG_COLOR[log.type] }}>
            {log.text}
          </div>
        ))}
      </div>
    </div>
  );
}

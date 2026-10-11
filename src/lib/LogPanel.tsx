/**
 * LogPanel — shared log section UI used by every batch tab.
 *
 * Renders the standard "Log" header (title + Clear button) plus the
 * scroll body containing readable entries with small status markers.
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

const LOG_MARKERS: Record<LogEntry["type"], { symbol: string; label: string }> = {
  error: { symbol: "×", label: "log_level_error" },
  warn: { symbol: "!", label: "log_level_warning" },
  success: { symbol: "✓", label: "log_level_success" },
  info: { symbol: "i", label: "log_level_info" },
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
        <button
          type="button"
          onClick={onClear}
          className="text-xs"
          style={{ color: "var(--text-muted)" }}
        >
          {t("log_clear")}
        </button>
      </div>
      <div ref={scrollRef} className="max-h-48 overflow-y-auto p-3 font-mono text-xs space-y-0.5">
        {logs.map((log) => (
          <div key={log.id} className={`log-entry log-entry-${log.type}`}>
            <span className="log-marker" aria-hidden="true">
              {LOG_MARKERS[log.type].symbol}
            </span>
            <span className="sr-only">{t(LOG_MARKERS[log.type].label)}: </span>
            <span>{log.text}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

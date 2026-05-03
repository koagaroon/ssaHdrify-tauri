import { useCallback, useEffect, useId, useRef, useState } from "react";
import {
  cancelFontScan,
  pickFontDirectory,
  pickFontFiles,
  preflightFontDirectory,
  preflightFontFiles,
  scanFontDirectory,
  scanFontFiles,
  type FontScanPreflight,
  type FontScanReason,
} from "../../lib/tauri-api";
import { ask } from "@tauri-apps/plugin-dialog";
import { useI18n } from "../../i18n/useI18n";
import type { FontUsage } from "./font-collector";
import { fontKeyLabel } from "./font-collector";
import { userFontKey } from "./font-embedder";
import { formatFontScanBytes, shouldWarnLargeFontScan } from "./font-source-warning";

export interface FontSource {
  /** Stable id used as a React key and for removal. */
  id: string;
  /** "dir" = picked a folder, "files" = picked individual files. */
  kind: "dir" | "files";
  /** Display label: folder basename or "N files". */
  label: string;
  /** Number of font faces this source contributed after dedup. */
  count: number;
}

/** Diagnostic Rust returns after attempting to add a source. */
export interface AddSourceResult {
  /** How many entries made it into the source list (after dedup). */
  added: number;
  /** How many entries were filtered out because they were already loaded. */
  duplicated: number;
}

interface Props {
  open: boolean;
  onClose: () => void;
  sources: FontSource[];
  usages: FontUsage[];
  localCoveredKeys: Set<string>;
  hasSubtitle: boolean;
  onAddSource: (source: FontSource) => void;
  onRemoveSource: (id: string) => void;
  /**
   * Lift the modal's scanning state to the parent so the parent ✕ Clear
   * button (and any future source-mutating control outside this modal)
   * can join the same lock. Without this, the parent doesn't know a scan
   * is mid-flight and would let the user click Clear → Rust rejects via
   * `reject_during_active_scan` → user sees a generic error log.
   */
  onScanStateChange?: (scanning: boolean) => void;
}

function basename(path: string): string {
  return path.replace(/\\/g, "/").split("/").filter(Boolean).pop() ?? path;
}

function newSourceId(): string {
  // Source ids are private opaque tokens used as the SQLite
  // `font_sources.source_id` primary key. The previous `Date.now()`
  // + random6 scheme was already collision-safe in practice; UUID is
  // cleaner / standard / lets the primary key get a single canonical
  // opaque format.
  //
  // crypto.randomUUID requires a secure context (Tauri's app:// scheme
  // qualifies; http:// would not). Defensive `?.` + fallback covers a
  // hypothetical future packaging change that ever served the bundle
  // over plain http; today it's belt-and-braces.
  return crypto.randomUUID?.() ?? `${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
}

// Seeded with Date.now() so a process restart won't collide with an
// in-flight cancel from a previous instance addressed at a stale id.
// The only invariant the seed must satisfy is "above NO_SCAN_ID = 0";
// monotonic increment from there is what `font_scan_cancelled` keys on.
let nextFontScanId = Date.now();
function newScanId(): number {
  nextFontScanId += 1;
  return nextFontScanId;
}

/** Compute how many required font families are currently resolved from local sources. */
function computeCoverage(
  usages: FontUsage[],
  localCoveredKeys: Set<string>,
  hasSubtitle: boolean
): { covered: number; total: number; missing: string[] } {
  if (!hasSubtitle || usages.length === 0) {
    return { covered: 0, total: 0, missing: [] };
  }
  let covered = 0;
  const missing: string[] = [];
  for (const u of usages) {
    const k = userFontKey(u.key.family, u.key.bold, u.key.italic);
    if (localCoveredKeys.has(k)) {
      covered += 1;
    } else {
      missing.push(fontKeyLabel(u.key));
    }
  }
  return { covered, total: usages.length, missing };
}

export default function FontSourceModal(props: Props) {
  const {
    open,
    onClose,
    sources,
    usages,
    localCoveredKeys,
    hasSubtitle,
    onAddSource,
    onRemoveSource,
    onScanStateChange,
  } = props;
  const { t } = useI18n();

  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const [scanning, setScanning] = useState(false);
  // Wrap setScanning to also notify the parent. Single source of truth
  // for "is the modal scanning" — every setter now goes through here so
  // the parent lock can never drift out of sync.
  const setScanningWithParent = useCallback(
    (next: boolean) => {
      setScanning(next);
      onScanStateChange?.(next);
    },
    [onScanStateChange]
  );
  // Live count for the "Scanned N fonts so far…" progress row. Rust can
  // deliver many Channel batches in a burst, so state updates are throttled
  // to one per animation frame; the heavy font-source index stays in Rust.
  const [scanProgress, setScanProgress] = useState(0);
  const scanProgressLatestRef = useRef(0);
  const scanProgressFrameRef = useRef<number | null>(null);
  // Rust cancellation is targeted by scan id, so late cancel commands from a
  // previous run cannot affect the next scan.
  const activeScanIdRef = useRef<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  // info is non-error feedback ("added N fonts") shown in a neutral tone.
  const [info, setInfo] = useState<string | null>(null);

  // a11y: stable id to wire `aria-labelledby` from the dialog div to the
  // visible title element. useId is React-stable across renders.
  const titleId = useId();
  // a11y: initial-focus target on open. The close button is a safe default —
  // it's always present, doesn't trigger a destructive action on Enter, and
  // gives keyboard users an obvious starting point. Full focus-trap is not
  // implemented; the modal is short enough that Tab cycling out is a known
  // limitation rather than a usability blocker.
  const closeButtonRef = useRef<HTMLButtonElement | null>(null);

  const requestClose = useCallback(() => {
    if (busyRef.current) return;
    onClose();
  }, [onClose]);

  // Close on Escape.
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") requestClose();
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [open, requestClose]);

  // Move keyboard focus into the modal on open so screen readers and
  // keyboard-only users land on a sensible starting point. Without this,
  // Tab would still address the focused element behind the scrim.
  useEffect(() => {
    if (!open) return;
    closeButtonRef.current?.focus();
  }, [open]);

  // Reset transient messages whenever the modal reopens.
  useEffect(() => {
    if (open) {
      setError(null);
      setInfo(null);
    }
  }, [open]);

  const flushScanProgress = useCallback(() => {
    scanProgressFrameRef.current = null;
    setScanProgress(scanProgressLatestRef.current);
  }, []);

  const scheduleScanProgress = useCallback(
    (total: number) => {
      scanProgressLatestRef.current = total;
      if (scanProgressFrameRef.current !== null) return;
      scanProgressFrameRef.current = requestAnimationFrame(flushScanProgress);
    },
    [flushScanProgress]
  );

  const resetScanProgress = useCallback(() => {
    scanProgressLatestRef.current = 0;
    if (scanProgressFrameRef.current !== null) {
      cancelAnimationFrame(scanProgressFrameRef.current);
      scanProgressFrameRef.current = null;
    }
    setScanProgress(0);
  }, []);

  useEffect(() => {
    return () => {
      if (scanProgressFrameRef.current !== null) {
        cancelAnimationFrame(scanProgressFrameRef.current);
      }
    };
  }, []);

  // Apply the dedup result consistently across folder and file picks.
  const applyAddResult = useCallback(
    (result: AddSourceResult) => {
      if (result.added === 0 && result.duplicated > 0) {
        setError(t("font_sources_all_duplicate"));
        setInfo(null);
      } else if (result.duplicated > 0) {
        setError(null);
        setInfo(t("font_sources_partial_duplicate", result.added, result.duplicated));
      } else {
        setError(null);
        setInfo(t("font_sources_added", result.added));
      }
    },
    [t]
  );

  // Compose the post-scan info message. When Rust reports an early stop AND
  // some entries were duplicates, fold both facts into a single message
  // instead of letting the stop notice clobber the dedup notice. The
  // three-way switch on `reason` replaces the prior (cancelled, ceilingHit)
  // boolean pair — see fonts::ScanStopReason for the wire contract.
  const reportSourceAdded = useCallback(
    (result: AddSourceResult, reason: FontScanReason) => {
      switch (reason) {
        case "ceilingHit":
          setError(null);
          setInfo(t("font_scan_ceiling_hit", result.added));
          return;
        case "userCancel":
          setError(null);
          if (result.duplicated > 0) {
            setInfo(t("font_scan_cancelled_with_dupes", result.added, result.duplicated));
          } else {
            setInfo(t("font_scan_cancelled", result.added));
          }
          return;
        case "natural":
          applyAddResult(result);
          return;
      }
    },
    [t, applyAddResult]
  );

  const handleCancelScan = useCallback(() => {
    const scanId = activeScanIdRef.current;
    if (scanId === null) return;
    // .catch — visible state stays correct because the running scan
    // checks font_scan_cancelled independently, but a real bug in the
    // cancel pathway (command not registered, arg shape drift) would
    // otherwise be invisible. Dev-visibility only; no user-facing error.
    cancelFontScan(scanId).catch((e: unknown) => {
      console.warn("cancelFontScan failed:", e);
    });
  }, []);

  const claimScanFlow = useCallback(() => {
    if (busyRef.current) return false;
    busyRef.current = true;
    setBusy(true);
    return true;
  }, []);

  const releaseScanFlow = useCallback(() => {
    busyRef.current = false;
    setBusy(false);
  }, []);

  const confirmLargeFontScan = useCallback(
    async (preflight: FontScanPreflight): Promise<boolean> => {
      if (!shouldWarnLargeFontScan(preflight)) return true;
      return ask(
        t(
          "font_scan_large_warning",
          preflight.fontFiles,
          formatFontScanBytes(preflight.totalBytes)
        ),
        { title: t("font_scan_large_warning_title"), kind: "warning" }
      );
    },
    [t]
  );

  const handleAddFolder = useCallback(async () => {
    if (!claimScanFlow()) return;
    setError(null);
    setInfo(null);
    let scanId: number | null = null;
    try {
      const dir = await pickFontDirectory(t);
      if (!dir) return;
      const preflight = await preflightFontDirectory(dir);
      const confirmed = await confirmLargeFontScan(preflight);
      if (!confirmed) return;
      scanId = newScanId();
      const sourceId = newSourceId();
      activeScanIdRef.current = scanId;
      resetScanProgress();
      setScanningWithParent(true);
      const scan = await scanFontDirectory(dir, sourceId, scanId, (total) =>
        scheduleScanProgress(total)
      );
      if (scan.added === 0) {
        // Early stop before any face was parsed — distinguish ceiling hit
        // (source too large), user cancel, all-duplicate, and "folder
        // genuinely has no fonts" so the user knows what happened.
        if (scan.reason === "ceilingHit") {
          setInfo(t("font_scan_ceiling_hit", 0));
        } else if (scan.reason === "userCancel") {
          setInfo(t("font_scan_cancelled", 0));
        } else if (scan.duplicated > 0) {
          setError(t("font_sources_all_duplicate"));
        } else {
          setError(t("font_sources_no_fonts_in_folder", basename(dir)));
        }
        return;
      }
      onAddSource({
        id: sourceId,
        kind: "dir",
        label: basename(dir),
        count: scan.added,
      });
      const result = { added: scan.added, duplicated: scan.duplicated };
      reportSourceAdded(result, scan.reason);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (scanId !== null && activeScanIdRef.current === scanId) {
        activeScanIdRef.current = null;
      }
      setScanningWithParent(false);
      releaseScanFlow();
    }
  }, [
    onAddSource,
    t,
    claimScanFlow,
    confirmLargeFontScan,
    releaseScanFlow,
    reportSourceAdded,
    resetScanProgress,
    scheduleScanProgress,
    setScanningWithParent,
  ]);

  const handleAddFiles = useCallback(async () => {
    if (!claimScanFlow()) return;
    setError(null);
    setInfo(null);
    let scanId: number | null = null;
    try {
      const paths = await pickFontFiles(t);
      if (!paths || paths.length === 0) return;
      const preflight = await preflightFontFiles(paths);
      const confirmed = await confirmLargeFontScan(preflight);
      if (!confirmed) return;
      scanId = newScanId();
      const sourceId = newSourceId();
      activeScanIdRef.current = scanId;
      resetScanProgress();
      setScanningWithParent(true);
      const scan = await scanFontFiles(paths, sourceId, scanId, (total) =>
        scheduleScanProgress(total)
      );
      if (scan.added === 0) {
        if (scan.reason === "ceilingHit") {
          setInfo(t("font_scan_ceiling_hit", 0));
        } else if (scan.reason === "userCancel") {
          setInfo(t("font_scan_cancelled", 0));
        } else if (scan.duplicated > 0) {
          setError(t("font_sources_all_duplicate"));
        } else {
          setError(t("font_sources_no_fonts_in_files", paths.length));
        }
        return;
      }
      onAddSource({
        id: sourceId,
        kind: "files",
        label: t("font_sources_files_entry", paths.length, scan.added),
        count: scan.added,
      });
      const result = { added: scan.added, duplicated: scan.duplicated };
      reportSourceAdded(result, scan.reason);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (scanId !== null && activeScanIdRef.current === scanId) {
        activeScanIdRef.current = null;
      }
      setScanningWithParent(false);
      releaseScanFlow();
    }
  }, [
    onAddSource,
    t,
    claimScanFlow,
    confirmLargeFontScan,
    releaseScanFlow,
    reportSourceAdded,
    resetScanProgress,
    scheduleScanProgress,
    setScanningWithParent,
  ]);

  // Coverage: how many required families are matched by loaded local sources.
  // In this modal we only consider local matches, so the count reflects the user's question:
  // "does the folder I picked cover every font the ASS needs?" System-
  // installed matches are shown as secondary info in the main font list.
  const { covered, total, missing } = computeCoverage(usages, localCoveredKeys, hasSubtitle);

  if (!open) return null;

  const coverageComplete = total > 0 && covered === total;

  return (
    <div
      className="modal-scrim"
      onClick={(e) => {
        if (e.target === e.currentTarget) requestClose();
      }}
    >
      <div
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        // The dialog itself is keyboard-focusable so the initial-focus
        // useEffect lands somewhere predictable even before the close
        // button mounts on slow renders.
        tabIndex={-1}
      >
        {/* ── Header — title + subtitle + close ──── */}
        <div className="modal-head">
          <div className="modal-head-text">
            <div id={titleId} className="modal-title">
              {t("font_sources_title")}
            </div>
            <div className="modal-sub">{t("font_sources_modal_sub")}</div>
          </div>
          <button
            ref={closeButtonRef}
            type="button"
            onClick={requestClose}
            disabled={busy}
            className="modal-close"
            title={busy ? t("font_sources_scanning") : t("font_sources_close")}
            aria-label={t("font_sources_close")}
          >
            <svg
              width="16"
              height="16"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
              aria-hidden="true"
            >
              <path d="M18 6 6 18M6 6l12 12" />
            </svg>
          </button>
        </div>

        {/* ── Body — source list + option cards + status + coverage ── */}
        <div className="modal-body">
          {/* Existing sources */}
          {sources.length === 0 ? (
            <p className="text-xs" style={{ color: "var(--text-secondary)" }}>
              {t("font_sources_empty_hint")}
            </p>
          ) : (
            <ul
              className="rounded-lg overflow-hidden"
              style={{ border: "1px solid var(--border-light)" }}
            >
              {sources.map((src) => {
                const label =
                  src.kind === "dir"
                    ? t("font_sources_folder_entry", src.label, src.count)
                    : src.label;
                return (
                  <li
                    key={src.id}
                    className="flex items-center justify-between px-3 py-2 text-sm"
                    style={{
                      borderBottom:
                        "1px solid color-mix(in srgb, var(--border-light) 50%, transparent)",
                      color: "var(--text-primary)",
                    }}
                  >
                    <span className="truncate mr-3">{label}</span>
                    <button
                      type="button"
                      onClick={() => onRemoveSource(src.id)}
                      disabled={busy}
                      className="px-2 py-0.5 rounded text-xs"
                      style={{
                        background: "var(--cancel-bg)",
                        color: "var(--cancel-text)",
                        opacity: busy ? 0.5 : 1,
                        cursor: busy ? "not-allowed" : "pointer",
                      }}
                      title={busy ? t("font_sources_scanning") : t("font_sources_remove")}
                    >
                      ✕
                    </button>
                  </li>
                );
              })}
            </ul>
          )}

          {/* Option cards — two picker entry points */}
          <button type="button" onClick={handleAddFolder} disabled={busy} className="modal-opt">
            <span className="modal-opt-icon" aria-hidden="true">
              <svg
                width="20"
                height="20"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
              >
                <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z" />
              </svg>
            </span>
            <div className="modal-opt-text">
              <div className="modal-opt-title">
                {busy ? t("font_sources_scanning") : t("font_sources_add_folder")}
              </div>
              <div className="modal-opt-sub">{t("font_sources_add_folder_sub")}</div>
            </div>
          </button>
          <button type="button" onClick={handleAddFiles} disabled={busy} className="modal-opt">
            <span className="modal-opt-icon" aria-hidden="true">
              <svg
                width="20"
                height="20"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
              >
                <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8l-6-6z" />
                <path d="M14 2v6h6" />
              </svg>
            </span>
            <div className="modal-opt-text">
              <div className="modal-opt-title">
                {busy ? t("font_sources_scanning") : t("font_sources_add_files")}
              </div>
              <div className="modal-opt-sub">{t("font_sources_add_files_sub")}</div>
            </div>
          </button>

          {scanning && (
            <div
              className="rounded-lg px-3 py-2 flex items-center justify-between gap-3"
              style={{
                border: "1px solid var(--border-light)",
                background: "var(--bg-panel)",
                color: "var(--text-primary)",
              }}
              role="status"
              aria-live="polite"
            >
              <span className="text-sm">{t("font_scan_progress", scanProgress)}</span>
              <button
                type="button"
                onClick={handleCancelScan}
                className="btn-cancel-pill px-2 py-0.5 rounded text-xs"
              >
                {t("font_scan_cancel")}
              </button>
            </div>
          )}

          {error && (
            <p className="text-xs" style={{ color: "var(--error)" }}>
              {error}
            </p>
          )}

          {info && !error && (
            <p className="text-xs" style={{ color: "var(--success)" }}>
              {info}
            </p>
          )}

          {/* Coverage panel */}
          <div
            className="rounded-lg px-3 py-3"
            style={{
              border: "1px solid var(--border-light)",
              background: "var(--bg-panel)",
            }}
          >
            {!hasSubtitle ? (
              <p className="text-xs" style={{ color: "var(--text-secondary)" }}>
                {t("font_coverage_no_subtitle")}
              </p>
            ) : (
              <div className="space-y-2">
                <p
                  className="text-sm font-medium"
                  style={{
                    color: coverageComplete ? "var(--badge-green-text)" : "var(--text-primary)",
                  }}
                >
                  {t("font_coverage", covered, total)}
                  {coverageComplete && (
                    <span className="ml-2 badge badge-green">{t("font_coverage_complete")}</span>
                  )}
                </p>
                {missing.length > 0 && (
                  <>
                    <p className="text-xs" style={{ color: "var(--text-secondary)" }}>
                      {t("font_coverage_missing", missing.join(", "))}
                    </p>
                    <p className="text-xs" style={{ color: "var(--text-muted)" }}>
                      {t("font_coverage_hint")}
                    </p>
                  </>
                )}
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

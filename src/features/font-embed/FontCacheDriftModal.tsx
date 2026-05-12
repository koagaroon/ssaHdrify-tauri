import { useCallback, useEffect, useId, useRef, useState } from "react";
import {
  clearFontCache,
  rescanFontCacheDrift,
  type FontCacheDriftReport,
  type FontCacheSkippedFolder,
  type FontCacheStatus,
} from "../../lib/tauri-api";
import { useI18n } from "../../i18n/useI18n";

interface Props {
  /** True when the parent has detected drift OR a schema mismatch and
   *  wants the user to choose. Modal stays mounted while open=true and
   *  un-mounts when the parent flips it to false. */
  open: boolean;
  /** Latest cache status. `schemaMismatch=true` puts the modal into
   *  "rebuild required" mode (only Clear cache is offered). */
  status: FontCacheStatus | null;
  /** Drift report. Empty when status.schemaMismatch is true (no cache to
   *  diff against). At least one of `modified` / `removed` is non-empty
   *  when the modal is shown for drift. */
  drift: FontCacheDriftReport | null;
  /** "Use as-is" — also fires on Esc / scrim / ✕. Cache stays as-is;
   *  embed will use stale entries until next launch's drift check. */
  onClose: () => void;
  /** Fires when Rescan completes successfully. Parent should re-run
   *  detect_drift and clear `drift` state. The modal stays mounted
   *  showing the "Rescanned N folders" success line until the user
   *  dismisses (X / scrim / Use as-is) — Round 2 changed this from
   *  the original auto-close so the result count is visible. */
  onRescanComplete: () => void;
  /** Fires when Clear completes successfully. Parent should re-run
   *  open_font_cache to refresh status. Modal stays open showing
   *  the "Font cache cleared" success line until user dismisses
   *  (same Round 2 design change as onRescanComplete). */
  onClearComplete: () => void;
}

export default function FontCacheDriftModal({
  open,
  status,
  drift,
  onClose,
  onRescanComplete,
  onClearComplete,
}: Props): React.ReactElement | null {
  const { t } = useI18n();
  const titleId = useId();
  const closeButtonRef = useRef<HTMLButtonElement | null>(null);

  // Single mutex across both async actions. Either both buttons are
  // enabled or both are disabled; never one-but-not-the-other.
  const [working, setWorking] = useState<null | "rescanning" | "clearing">(null);
  const [error, setError] = useState<string | null>(null);
  // Surface the post-op success message inline rather than auto-closing
  // silently — the i18n strings font_cache_rescan_done /
  // font_cache_cleared exist precisely so the user sees what happened.
  // null while the modal is in pre-op or working state.
  const [doneMessage, setDoneMessage] = useState<string | null>(null);
  // Phase-2 skipped folders from the last rescan. Rendered as a
  // partial-success block beneath the doneMessage so the user knows
  // which folders couldn't be refreshed (rows already evicted Rust-side).
  const [skippedFolders, setSkippedFolders] = useState<FontCacheSkippedFolder[]>([]);

  // Esc closes only when not working — closing mid-rescan would orphan
  // the in-flight Tauri command. The close button's disabled state
  // matches.
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape" && working === null) {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", handler, true);
    return () => window.removeEventListener("keydown", handler, true);
  }, [open, working, onClose]);

  // Land focus on the close button on mount so keyboard users can
  // tab from a known anchor. Predictable initial focus matches
  // FontSourceModal's pattern.
  useEffect(() => {
    if (open) closeButtonRef.current?.focus();
  }, [open]);

  const requestClose = useCallback(() => {
    if (working !== null) return;
    onClose();
  }, [working, onClose]);

  const handleRescan = useCallback(async () => {
    // Reset transient states at entry — a prior op's doneMessage or
    // skipped list would otherwise persist next to the in-progress
    // "Rescanning…" banner until rescan finishes.
    setError(null);
    setDoneMessage(null);
    setSkippedFolders([]);
    setWorking("rescanning");
    try {
      const result = await rescanFontCacheDrift();
      setWorking(null);
      setDoneMessage(t("font_cache_rescan_done", result.modifiedRescanned, result.removedEvicted));
      setSkippedFolders(result.skipped);
      onRescanComplete();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setWorking(null);
    }
  }, [onRescanComplete, t]);

  const handleClear = useCallback(async () => {
    setError(null);
    setDoneMessage(null);
    setSkippedFolders([]);
    setWorking("clearing");
    try {
      await clearFontCache();
      setWorking(null);
      setDoneMessage(t("font_cache_cleared"));
      onClearComplete();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setWorking(null);
    }
  }, [onClearComplete, t]);

  // Reset transient state on every open=false→true transition. App.tsx
  // currently mounts/unmounts the modal so this is defense for a future
  // refactor that keeps it mounted — without this, a stale doneMessage
  // from a prior open would leak into the next drift report.
  useEffect(() => {
    if (open) {
      setError(null);
      setDoneMessage(null);
      setSkippedFolders([]);
      setWorking(null);
    }
  }, [open]);

  if (!open) return null;

  const schemaMismatch = status?.schemaMismatch === true;
  const modifiedCount = drift?.modified.length ?? 0;
  const removedCount = drift?.removed.length ?? 0;
  const totalChanged = modifiedCount + removedCount;

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
        tabIndex={-1}
      >
        {/* ── Header ── */}
        <div className="modal-head">
          <div className="modal-head-text">
            <div id={titleId} className="modal-title">
              {schemaMismatch
                ? t("font_cache_rebuild_required_title")
                : t("font_cache_drift_title")}
            </div>
          </div>
          <button
            ref={closeButtonRef}
            type="button"
            onClick={requestClose}
            disabled={working !== null}
            className="modal-close"
            title={t("font_cache_drift_btn_use_as_is")}
            aria-label={t("font_cache_drift_btn_use_as_is")}
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

        {/* ── Body ── */}
        <div className="modal-body">
          {schemaMismatch ? (
            <p className="text-sm" style={{ color: "var(--text-primary)" }}>
              {t("font_cache_rebuild_required_body")}
            </p>
          ) : (
            <>
              <p className="text-sm" style={{ color: "var(--text-primary)" }}>
                {t("font_cache_drift_summary", totalChanged, modifiedCount, removedCount)}
              </p>

              {modifiedCount > 0 && (
                <div className="text-xs" style={{ color: "var(--text-secondary)" }}>
                  <div style={{ marginTop: "0.5rem", marginBottom: "0.25rem" }}>
                    {t("font_cache_drift_modified_label")}
                  </div>
                  <ul style={{ paddingLeft: "1rem", listStyle: "disc" }}>
                    {drift!.modified.slice(0, 8).map((p) => (
                      <li key={p} style={{ wordBreak: "break-all" }}>
                        {p}
                      </li>
                    ))}
                    {drift!.modified.length > 8 && <li>… +{drift!.modified.length - 8}</li>}
                  </ul>
                </div>
              )}

              {removedCount > 0 && (
                <div className="text-xs" style={{ color: "var(--text-secondary)" }}>
                  <div style={{ marginTop: "0.5rem", marginBottom: "0.25rem" }}>
                    {t("font_cache_drift_removed_label")}
                  </div>
                  <ul style={{ paddingLeft: "1rem", listStyle: "disc" }}>
                    {drift!.removed.slice(0, 8).map((p) => (
                      <li key={p} style={{ wordBreak: "break-all" }}>
                        {p}
                      </li>
                    ))}
                    {drift!.removed.length > 8 && <li>… +{drift!.removed.length - 8}</li>}
                  </ul>
                </div>
              )}

              <p
                className="text-xs"
                role="note"
                style={{
                  marginTop: "0.75rem",
                  color: "var(--text-secondary)",
                  fontStyle: "italic",
                }}
              >
                {t("font_cache_drift_close_hint")}
              </p>
            </>
          )}

          {working !== null && (
            <p
              className="text-xs"
              role="status"
              aria-live="polite"
              style={{ marginTop: "0.5rem", color: "var(--text-primary)" }}
            >
              {working === "rescanning" ? t("font_cache_rescanning") : t("font_cache_clearing")}
            </p>
          )}

          {doneMessage !== null && (
            <p
              className="text-xs"
              role="status"
              aria-live="polite"
              style={{ marginTop: "0.5rem", color: "var(--text-primary)" }}
            >
              {doneMessage}
            </p>
          )}

          {skippedFolders.length > 0 && (
            <div
              className="text-xs"
              role="status"
              aria-live="polite"
              style={{ marginTop: "0.5rem", color: "var(--text-primary)" }}
            >
              <div style={{ marginBottom: "0.25rem" }}>
                {t("font_cache_rescan_skipped_label", skippedFolders.length)}
              </div>
              <ul
                style={{ paddingLeft: "1rem", listStyle: "disc", color: "var(--text-secondary)" }}
              >
                {skippedFolders.slice(0, 8).map((sk) => (
                  <li key={sk.folder} style={{ wordBreak: "break-all" }}>
                    <span style={{ color: "var(--text-primary)" }}>{sk.folder}</span>
                    {" — "}
                    {sk.reason}
                  </li>
                ))}
                {skippedFolders.length > 8 && <li>… +{skippedFolders.length - 8}</li>}
              </ul>
            </div>
          )}

          {error !== null && (
            <p
              className="text-xs"
              role="alert"
              style={{ marginTop: "0.5rem", color: "var(--text-danger)" }}
            >
              {error}
            </p>
          )}
        </div>

        {/* ── Actions ── */}
        {/* Drift mode: Rescan + Use-as-is + Clear (3 buttons).
            Schema-mismatch mode: Clear only (1 button). The asymmetry is
            intentional — rescan/use-as-is have no meaning when the cache
            file's schema doesn't match this build. Keyboard tab order
            shifts accordingly; close button (X) at top is always reachable. */}
        <div
          className="modal-body"
          style={{
            display: "flex",
            justifyContent: "flex-end",
            gap: "0.5rem",
            paddingTop: 0,
          }}
        >
          {!schemaMismatch && (
            <button
              type="button"
              onClick={handleRescan}
              disabled={working !== null || totalChanged === 0}
              className="px-3 py-1.5 rounded text-sm"
              style={{
                background: "var(--accent-bg, #6e56cf)",
                color: "var(--accent-text, white)",
                border: "1px solid var(--accent-border, #6e56cf)",
                cursor: working !== null ? "not-allowed" : "pointer",
                filter: working !== null ? "grayscale(1)" : "none",
              }}
            >
              {t("font_cache_drift_btn_rescan")}
            </button>
          )}
          {!schemaMismatch && (
            <button
              type="button"
              onClick={requestClose}
              disabled={working !== null}
              className="px-3 py-1.5 rounded text-sm"
              style={{
                background: "transparent",
                color: "var(--text-primary)",
                border: "1px solid var(--border-light)",
                cursor: working !== null ? "not-allowed" : "pointer",
              }}
            >
              {t("font_cache_drift_btn_use_as_is")}
            </button>
          )}
          <button
            type="button"
            onClick={handleClear}
            disabled={working !== null}
            className="px-3 py-1.5 rounded text-sm"
            style={{
              background: "var(--cancel-bg)",
              color: "var(--cancel-text)",
              border: "1px solid var(--cancel-border, var(--border-light))",
              cursor: working !== null ? "not-allowed" : "pointer",
              filter: working !== null ? "grayscale(1)" : "none",
            }}
          >
            {t("font_cache_drift_btn_clear")}
          </button>
        </div>
      </div>
    </div>
  );
}

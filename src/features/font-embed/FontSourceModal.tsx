import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
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
  type FontScanResult,
} from "../../lib/tauri-api";
import { ask } from "@tauri-apps/plugin-dialog";
import { sanitizeError, sanitizeForDialog } from "../../lib/dedup-helpers";
import { isWindowsRuntime } from "../../lib/platform";
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
  // Backslash → forward only on Windows (Round 8 POSIX-correctness gate,
  // parity with the four sibling helpers fixed in Wave 8.1). On POSIX
  // `\` is a valid filename character; a folder literally named `a\b`
  // must display as `a\b`, not as `b`.
  const norm = isWindowsRuntime ? path.replace(/\\/g, "/") : path;
  return norm.split("/").filter(Boolean).pop() ?? path;
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
  // over plain http; today it's belt-and-braces. The fallback uses
  // the canonical UUIDv4 hex/dash shape (8-4-4-4-12) so any consumer
  // logging the id sees a uniform format regardless of which path
  // produced it.
  return crypto.randomUUID?.() ?? fallbackUuidV4();
}

function fallbackUuidV4(): string {
  // Manual UUIDv4 from Math.random — only used when crypto.randomUUID
  // isn't available. Not cryptographically strong (collision-safe is
  // enough for an opaque session-scoped primary key).
  const hex = (n: number) =>
    Math.floor(Math.random() * 16 ** n)
      .toString(16)
      .padStart(n, "0");
  // Per RFC 4122: version 4 is the literal "4" preceding the third
  // group; the VARIANT bits (10xx) live in the high nibble of the
  // fourth group (N-R5-FECHAIN-04 rename — old name was `version`
  // which conflicted with the literal "4" that's the actual version).
  const variant = (8 + Math.floor(Math.random() * 4)).toString(16); // 8/9/a/b
  return `${hex(8)}-${hex(4)}-4${hex(3)}-${variant}${hex(3)}-${hex(12)}`;
}

// Module-level singleton by design — the modal only ever mounts once
// in this app, AND a single monotonic counter across StrictMode
// double-mounts / future remounts is correctness-preserving (Rust
// rejects same-id cancel races and ACTIVE_SCAN_ID's CAS guards
// against concurrent scans). Switching to `useRef` per instance would
// risk reusing low ids on remount, so the singleton stays.
//
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
    // Invariant: onClose must NOT be called while busyRef is set
    // (other than via the cancel-then-close path below). Other busy
    // paths (e.g., the `applyAddResult` handlers in claim/release
    // flow) bracket their work with claimScanFlow/releaseScanFlow,
    // so by the time the user can click ✕ / scrim / Esc, busyRef
    // is either false (safe to close) or true with an active scan
    // (route to cancel).
    //
    // If a scan is in flight, route Esc / scrim click / ✕ button to a
    // cancel attempt instead of silently doing nothing. The user gets
    // an obvious dismiss path that doesn't require finding the inline
    // Cancel button. After the cancel settles, busyRef clears and a
    // second dismiss closes the modal normally.
    const activeScanId = activeScanIdRef.current;
    if (activeScanId !== null) {
      cancelFontScan(activeScanId).catch((e: unknown) => {
        // Surface the IPC failure inside the modal so the user understands
        // why the dismiss didn't take effect; without this they're stuck
        // with no feedback. Console line stays for dev diagnostics.
        // sanitizeForDialog scrubs BiDi / zero-width controls — Rust IPC
        // error strings can interpolate font-pack paths (P1b
        // attacker-influenced content), and React renders the result
        // directly into the modal banner without the BiDi reversal
        // protection that `validate_ipc_path` would have applied
        // upstream (Round 6 Wave 6.2 parity sweep).
        const message = sanitizeError(e);
        console.warn("cancelFontScan failed:", e);
        setError(t("font_scan_cancel_failed", message));
      });
      return;
    }
    if (busyRef.current) return;
    onClose();
  }, [onClose, t]);

  // Close on Escape. stopPropagation prevents the same Esc from also
  // dismissing any background dropdown / outer-scope listener that
  // captures Escape (e.g., a future picker overlay or future
  // command-palette key handler) — the modal is the topmost surface
  // when open and should consume the key exclusively.
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        requestClose();
      }
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
  // Callers guarantee `result.added > 0`: the all-duplicate case
  // (added === 0 && duplicated > 0) is short-circuited earlier in
  // `runScanFlow` (see line ~448 where `scan.reason === "natural"` AND
  // `scan.added === 0` lands on `font_sources_all_duplicate` directly),
  // so this helper only needs to render the success / partial-dupe
  // shapes.
  const applyAddResult = useCallback(
    (result: AddSourceResult) => {
      if (result.duplicated > 0) {
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

  // Tracks whether the user has clicked Cancel during the current
  // scan, so the inline cancel button can show a transitional
  // "Cancelling…" state until the scan worker actually settles
  // (drives release of the busy lock via setScanningWithParent(false)
  // in handleAddFolder/Files's finally). Without this the user
  // wonders if the click registered.
  const [cancelRequested, setCancelRequested] = useState(false);
  // Reset cancellation request whenever a new scan starts (scanning
  // flips false→true) so the next scan starts with a clean cancel UI.
  useEffect(() => {
    if (scanning) setCancelRequested(false);
  }, [scanning]);

  const handleCancelScan = useCallback(() => {
    const scanId = activeScanIdRef.current;
    if (scanId === null) return;
    setCancelRequested(true);
    // .catch — visible state stays correct because the running scan
    // checks font_scan_cancelled independently, but a real bug in the
    // cancel pathway (command not registered, arg shape drift) would
    // otherwise be invisible to the user (UI keeps spinning). Surface
    // through the modal's error banner so they see the IPC failed.
    cancelFontScan(scanId).catch((e: unknown) => {
      // sanitizeForDialog: same Round 6 Wave 6.2 parity reason as the
      // requestClose cancel path above — Rust IPC errors can carry
      // attacker-influenced path strings.
      const message = sanitizeError(e);
      console.warn("cancelFontScan failed:", e);
      setError(t("font_scan_cancel_failed", message));
    });
  }, [t]);

  const claimScanFlow = useCallback(() => {
    if (busyRef.current) return false;
    busyRef.current = true;
    setBusy(true);
    // Notify the parent IMMEDIATELY so its sourceLocked = sourceBusy ||
    // modalScanning evaluates true across the entire pick/preflight/
    // confirm window. Previously setScanningWithParent fired only after
    // confirmLargeFontScan returned (post-OS-picker), leaving a real
    // hole where the parent ✕ Clear button stayed enabled while the OS
    // picker was up — a user clicking Add Folder then ✕ Clear could
    // fire clearFontSources against state the modal was about to
    // mutate (N-R5-FECHAIN-01). Mirrored in releaseScanFlow.
    setScanningWithParent(true);
    return true;
  }, [setScanningWithParent]);

  const releaseScanFlow = useCallback(() => {
    // Ordering note: clearing busyRef synchronously while setBusy(false)
    // flushes async creates a sub-render-cycle window where busyRef is
    // false and activeScanIdRef is null while the spinner is still
    // visible. Worst case if the race fires: modal closes one paint
    // earlier than ideal — no data loss, no scan-state corruption
    // (busyRef is read by the next claim attempt, not by the close
    // itself). Single-user UI timing makes the window genuinely
    // unhittable by hand. Documented here so a future reader doesn't
    // assume it's a defect.
    busyRef.current = false;
    setBusy(false);
    setScanningWithParent(false);
  }, [setScanningWithParent]);

  const confirmLargeFontScan = useCallback(
    async (preflight: FontScanPreflight): Promise<boolean> => {
      if (!shouldWarnLargeFontScan(preflight)) return true;
      // tauri-plugin-dialog 2.x returns false for close-via-X on
      // Windows, matching "user declined" — that's what we want here.
      // Behavior is plugin-version-dependent; a future plugin upgrade
      // that switches close-via-X to throw / return undefined would
      // change "user closed window without confirming" from "abort
      // safely" to "treat as approval", so re-test this path after any
      // tauri-plugin-dialog bump.
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

  // Shared scan flow: pick → preflight → confirm large → scan → branch
  // on outcome. Folder and file-list flows previously inlined this same
  // 60-line skeleton with only the picker / preflight / scan / source-
  // builder / empty-error differing — easy to drift between the two.
  // The closure below captures all the scoped state (refs, setters,
  // setScanningWithParent) so the per-flow callsite stays a tiny
  // descriptor.
  const runScanFlow = useCallback(
    async function <T>(opts: {
      pickInput: () => Promise<T | null>;
      preflight: (input: T) => Promise<FontScanPreflight>;
      scan: (
        input: T,
        sourceId: string,
        scanId: number,
        onProgress: (total: number) => void
      ) => Promise<FontScanResult>;
      buildSource: (input: T, sourceId: string, addedCount: number) => FontSource;
      emptyError: (input: T) => string;
    }): Promise<void> {
      if (!claimScanFlow()) return;
      setError(null);
      setInfo(null);
      let scanId: number | null = null;
      try {
        const input = await opts.pickInput();
        if (input === null) return;
        const preflight = await opts.preflight(input);
        const confirmed = await confirmLargeFontScan(preflight);
        if (!confirmed) return;
        scanId = newScanId();
        const sourceId = newSourceId();
        activeScanIdRef.current = scanId;
        resetScanProgress();
        // setScanningWithParent(true) already fired inside claimScanFlow
        // — the parent lock spans the full pick/preflight/confirm/scan
        // arc, not just from-here-down.
        const scan = await opts.scan(input, sourceId, scanId, (total) =>
          scheduleScanProgress(total)
        );
        if (scan.added === 0) {
          // Early stop before any face was parsed — distinguish ceiling
          // hit (source too large), user cancel, all-duplicate, and the
          // genuinely-empty case so the user knows what happened.
          if (scan.reason === "ceilingHit") {
            setInfo(t("font_scan_ceiling_hit", 0));
          } else if (scan.reason === "userCancel") {
            // Cancel-before-any-face. If everything in the picked input
            // was a duplicate of an already-loaded source, fold that
            // signal into the cancel notice so it isn't lost — same
            // combination reportSourceAdded uses on the non-zero-added
            // path via font_scan_cancelled_with_dupes.
            if (scan.duplicated > 0) {
              setInfo(t("font_scan_cancelled_with_dupes", 0, scan.duplicated));
            } else {
              setInfo(t("font_scan_cancelled", 0));
            }
          } else if (scan.duplicated > 0) {
            setError(t("font_sources_all_duplicate"));
          } else {
            setError(opts.emptyError(input));
          }
          return;
        }
        onAddSource(opts.buildSource(input, sourceId, scan.added));
        const result = { added: scan.added, duplicated: scan.duplicated };
        reportSourceAdded(result, scan.reason);
      } catch (e) {
        // sanitizeForDialog: the catch covers the full scan pipeline
        // (picker / preflight / streaming scan), so the error message
        // can carry font-pack path strings or font-file names which
        // are attacker-influenced (P1b). Round 6 Wave 6.2.
        setError(sanitizeError(e));
      } finally {
        if (scanId !== null && activeScanIdRef.current === scanId) {
          activeScanIdRef.current = null;
        }
        // setScanningWithParent(false) is owned by releaseScanFlow now —
        // pairing it 1:1 with claimScanFlow keeps both lock transitions
        // in one place, so the parent unlock cannot be skipped on an
        // early `return` path above (e.g. picker cancelled).
        releaseScanFlow();
      }
    },
    [
      onAddSource,
      t,
      claimScanFlow,
      confirmLargeFontScan,
      releaseScanFlow,
      reportSourceAdded,
      resetScanProgress,
      scheduleScanProgress,
      // Round 7 Wave 7.7 (lint): setScanningWithParent is referenced
      // ONLY in comments inside runScanFlow's body — actual calls
      // happen via claimScanFlow / releaseScanFlow, which are already
      // in the dep array. eslint react-hooks/exhaustive-deps flagged
      // the explicit entry as unnecessary; removing it closes the
      // baseline warning carried since Round 5.
    ]
  );

  const handleAddFolder = useCallback(
    () =>
      runScanFlow<string>({
        pickInput: () => pickFontDirectory(t),
        preflight: (dir) => preflightFontDirectory(dir),
        scan: (dir, sourceId, scanId, onProgress) =>
          scanFontDirectory(dir, sourceId, scanId, onProgress),
        buildSource: (dir, sourceId, count) => ({
          id: sourceId,
          kind: "dir",
          label: basename(dir),
          count,
        }),
        emptyError: (dir) => t("font_sources_no_fonts_in_folder", basename(dir)),
      }),
    [runScanFlow, t]
  );

  const handleAddFiles = useCallback(
    () =>
      runScanFlow<string[]>({
        pickInput: async () => {
          const paths = await pickFontFiles(t);
          // Treat "0 picked" the same as "cancelled picker" — the
          // shared flow uses null to mean "no input, exit silently."
          return paths && paths.length > 0 ? paths : null;
        },
        preflight: (paths) => preflightFontFiles(paths),
        scan: (paths, sourceId, scanId, onProgress) =>
          scanFontFiles(paths, sourceId, scanId, onProgress),
        buildSource: (paths, sourceId, count) => ({
          id: sourceId,
          kind: "files",
          label: t("font_sources_files_entry", paths.length, count),
          count,
        }),
        emptyError: (paths) => t("font_sources_no_fonts_in_files", paths.length),
      }),
    [runScanFlow, t]
  );

  // Coverage: how many required families are matched by loaded local sources.
  // In this modal we only consider local matches, so the count reflects the user's question:
  // "does the folder I picked cover every font the ASS needs?" System-
  // installed matches are shown as secondary info in the main font list.
  // Memoize so re-renders triggered by transient state (info / error
  // strings, scan progress) don't redo the full usages × localCoveredKeys
  // walk. Only the inputs to computeCoverage actually invalidate the
  // result.
  const { covered, total, missing } = useMemo(
    () => computeCoverage(usages, localCoveredKeys, hasSubtitle),
    [usages, localCoveredKeys, hasSubtitle]
  );

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
            className="modal-close"
            title={busy ? t("font_scan_cancel") : t("font_sources_close")}
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
                // BiDi control sanitization (Round 1 F3.A-R1-8): a folder
                // name carrying RLO / LRO / PDF could visually reverse
                // adjacent characters in the source list, making
                // `evil.ttf` look benign. `sanitizeForDialog` is the
                // shared strip used in BatchRename's ask() dialogs.
                const safeLabel = sanitizeForDialog(src.label);
                const label =
                  src.kind === "dir"
                    ? t("font_sources_folder_entry", safeLabel, src.count)
                    : safeLabel;
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
                        filter: busy ? "grayscale(1)" : "none",
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
                disabled={cancelRequested}
                className="btn-cancel-pill px-2 py-0.5 rounded text-xs"
                style={{ filter: cancelRequested ? "grayscale(1)" : "none" }}
              >
                {cancelRequested ? t("font_scan_cancelling") : t("font_scan_cancel")}
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
                    {/* Round 7 Wave 7.6 (N3-R7-9): `missing` entries
                        come from `fontKeyLabel(u.key)` where `u.key.family`
                        was sanitized via `sanitizeFamily` upstream
                        (font-collector.ts), so the BiDi / zero-width
                        scrub already happened — no second wrap needed
                        at this render site. */}
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

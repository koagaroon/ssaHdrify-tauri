import { useState, useCallback, useEffect, useRef, useMemo } from "react";
import { List, type RowComponentProps } from "react-window";
import { pickSubtitleFiles, readText, writeText, fileNameFromPath } from "../../lib/tauri-api";
import { ask } from "@tauri-apps/plugin-dialog";
import {
  shiftSubtitles,
  formatDisplayTime,
  parseDisplayTime,
  deriveShiftedPath,
  type PreviewEntry,
} from "./timing-engine";
import { useI18n } from "../../i18n/useI18n";
import { useFileContext } from "../../lib/FileContext";
import type { Status } from "../../lib/StatusContext";
import { useTabStatus } from "../../lib/useTabStatus";
import { useFolderDrop } from "../../lib/useFolderDrop";
import { countExistingFiles } from "../../lib/output-collisions";
import { useClickOutside } from "../../lib/useClickOutside";
import { useLogPanel } from "../../lib/useLogPanel";
import { LogPanel } from "../../lib/LogPanel";
import { DropErrorBanner } from "../../lib/DropErrorBanner";
import NumberInput from "../../lib/NumberInput";
import {
  buildConflictMessage,
  normalizeOutputKey,
  sanitizeError,
  sanitizeForDialog,
} from "../../lib/dedup-helpers";

type Unit = "ms" | "s";
type Direction = "slower" | "faster";

/** Cap to ±1 year to prevent integer precision loss for extreme inputs. */
const MAX_OFFSET_MS = 365 * 24 * 3600 * 1000;

// Subtitle extensions Time Shift accepts. Used by the folder-drop filter
// to keep videos and unrelated files out of the batch when a user drops
// a whole show folder.
const SUBTITLE_EXTS = new Set(["ass", "ssa", "srt", "vtt", "sub", "sbv", "lrc"]);

function fileNameHasSubtitleExt(name: string): boolean {
  const dot = name.lastIndexOf(".");
  if (dot < 0) return false;
  return SUBTITLE_EXTS.has(name.slice(dot + 1).toLowerCase());
}

// Preview list virtualization (Codex d611ab66 / 7f04ebe6 — preview DOM
// explosion at the parser's 500k-entry ceiling). The previous
// preview.map(...) materialized one DOM node per caption regardless of
// scroll position, freezing the WebView on large inputs even though only
// a few rows fit in the 280px viewport at any time. react-window
// constrains the rendered set to overscan + visible rows.
//
// PREVIEW_ROW_HEIGHT: derived from the .timeline-row CSS — 12px mono
// text × 1.2 line-height + 6px top/bottom padding ≈ 26.4px; rounded up
// to 30 for a margin so descenders don't clip. If the row CSS ever
// changes, this constant must follow OR the preview will misalign.
const PREVIEW_ROW_HEIGHT = 30;
const PREVIEW_LIST_MAX_HEIGHT = 280;

interface PreviewRowData {
  preview: PreviewEntry[];
  formatTime: (ms: number) => string;
  origLabel: string;
  shiftedLabel: string;
}

function PreviewRow({
  index,
  style,
  ariaAttributes,
  preview,
  formatTime,
  origLabel,
  shiftedLabel,
}: RowComponentProps<PreviewRowData>) {
  const entry = preview[index];
  // Spread `ariaAttributes` (Round 1 F2.N-R1-24): react-window 2.x
  // supplies `role`, `aria-rowindex`, etc. via this prop and expects
  // the row component to apply them to the root element. Without the
  // spread, screen readers can't navigate the virtualized list as a
  // table.
  return (
    <div
      {...ariaAttributes}
      style={style}
      className={"timeline-row" + (entry.wasShifted ? "" : " unchanged")}
    >
      <span className="idx">{entry.index}</span>
      <span className="t-orig" title={`${origLabel}: ${formatTime(entry.originalStart)}`}>
        {formatTime(entry.originalStart)}
      </span>
      <span className="t-new" title={`${shiftedLabel}: ${formatTime(entry.shiftedStart)}`}>
        {formatTime(entry.shiftedStart)}
      </span>
      <span className="txt" title={entry.fullText}>
        {entry.text}
      </span>
    </div>
  );
}

export default function TimingShift() {
  const { t } = useI18n();
  const { timingFiles, setTimingFiles, clearFile, isFileInUse } = useFileContext();

  const [detectedFormat, setDetectedFormat] = useState<string>("");
  // Round 8 Wave 8.6 — closes N-R5-FEFEAT-24. Two-slot shadow mirroring
  // HdrConvert's `brightness` / `brightnessText`: `offsetValue` is the
  // validated integer used by `effectiveOffsetMs` math; `offsetText` is
  // the raw input string the user is typing. The pair lets NumberInput
  // own clear-then-retype semantics (mid-type NaN states don't snap the
  // visible field back to a stale number) while keeping the math path
  // typed.
  const [offsetValue, setOffsetValue] = useState(200);
  const [offsetText, setOffsetText] = useState("200");
  const [unit, setUnit] = useState<Unit>("ms");
  const [direction, setDirection] = useState<Direction>("slower");
  const [useThreshold, setUseThreshold] = useState(false);
  const [thresholdText, setThresholdText] = useState("00:05:00.000");
  const [preview, setPreview] = useState<PreviewEntry[]>([]);
  const [captionCount, setCaptionCount] = useState(0);
  const [busy, setBusy] = useState(false);
  // Same shape as HdrConvert — "cancelled" is its own visible state so
  // the footer and log can both acknowledge that the user stepped back
  // (overwrite-confirm dismissed OR mid-batch cancel button).
  const [lastActionResult, setLastActionResult] = useState<
    "success" | "error" | "cancelled" | null
  >(null);
  const [progress, setProgress] = useState<{ processed: number; total: number } | null>(null);
  const [dropActive, setDropActive] = useState(false);
  const { logs, addLog, clearLogs, logScrollRef } = useLogPanel();
  const [showFileList, setShowFileList] = useState(false);
  // Selection-rejection banner — see HdrConvert for the full rationale.
  // Strict cross-tab dedup: any conflict in the new selection rejects
  // the entire drop, leaving the prior state untouched.
  const [dropError, setDropError] = useState<string | null>(null);

  const debounceRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const pickGenRef = useRef(0);
  const abortRef = useRef<AbortController | null>(null);
  // Synchronous double-click guard — `busy` state lags setBusy(true) by
  // one render. busyRef is written synchronously at handler entry and
  // released in the outer finally so every exit path clears it.
  const busyRef = useRef(false);
  const dropZoneRef = useRef<HTMLDivElement>(null);
  const fileContainerRef = useRef<HTMLDivElement>(null);

  const effectiveOffsetMs = useMemo(() => {
    const base = unit === "s" ? offsetValue * 1000 : offsetValue;
    // Round 11 W11.1 (N1-R11-02): Math.round at the math boundary so
    // fractional s-unit inputs (e.g. "2.5s" → 2500 ms) accepted by
    // parseFloat below don't propagate sub-ms fractions into
    // formatSrtTime, whose `ms % 1000` produces non-integer
    // milliseconds that padStart can't format cleanly. Integer-ms is
    // the downstream contract.
    const rounded = Math.round(base);
    const clamped = Math.max(-MAX_OFFSET_MS, Math.min(MAX_OFFSET_MS, rounded));
    return direction === "faster" ? -clamped : clamped;
  }, [unit, offsetValue, direction]);

  // Per-unit absolute bound used by NumberInput's min/max attributes and
  // the out-of-range derived signal below. In ms-unit the cap is the full
  // MAX_OFFSET_MS literal; in s-unit it scales down by 1000 so the same
  // visible value range stays user-friendly.
  const offsetMax = unit === "s" ? MAX_OFFSET_MS / 1000 : MAX_OFFSET_MS;
  const handleOffsetChange = (value: string) => {
    setOffsetText(value);
    // Round 11 W11.1 (N1-R11-02): parseFloat accepts fractional s-unit
    // inputs ("2.5s" → 2.5 seconds). Pre-R11 parseInt silently dropped
    // the decimal portion ("2.5" → 2), violating no-silent-action.
    // effectiveOffsetMs rounds at the math boundary to keep integer-ms
    // downstream.
    const n = parseFloat(value);
    if (!Number.isNaN(n) && Math.abs(n) <= offsetMax) {
      setOffsetValue(n);
    }
  };
  // Derived: text parses cleanly but the magnitude exceeds the per-unit
  // cap. Without this, the math layer silently clamps in effectiveOffsetMs
  // and the user sees no feedback on their out-of-range input — the
  // same no-silent-action class HdrConvert's brightnessOutOfRange
  // addresses (N-R5-FEFEAT-25).
  const offsetOutOfRange = (() => {
    const n = parseFloat(offsetText);
    return !Number.isNaN(n) && Math.abs(n) > offsetMax;
  })();

  const thresholdMs = useMemo(
    () => (useThreshold ? parseDisplayTime(thresholdText) : null),
    [useThreshold, thresholdText]
  );
  const thresholdInvalid = useThreshold && thresholdMs === null;

  // Last caption's START time — shiftSubtitle uses `c.start >= threshold`,
  // so a threshold past the last START produces zero shifts even if it
  // falls before the last caption's END.
  const maxCaptionStart = useMemo(
    () => preview.reduce((max, e) => Math.max(max, e.originalStart), 0),
    [preview]
  );
  const thresholdExceedsFile =
    useThreshold && thresholdMs !== null && maxCaptionStart > 0 && thresholdMs > maxCaptionStart;

  const filePaths = useMemo(() => timingFiles?.filePaths ?? [], [timingFiles]);
  const fileNames = useMemo(() => timingFiles?.fileNames ?? [], [timingFiles]);
  const firstFileContent = timingFiles?.firstFileContent ?? "";
  const primaryFileName = fileNames[0] ?? "";
  const fileCount = filePaths.length;

  // Virtualized preview list height — shrinks with the actual row count
  // for short previews, caps at PREVIEW_LIST_MAX_HEIGHT for long ones.
  // Computed in a memo so the eslint inline-style rule (`no-restricted-syntax`)
  // sees an Identifier at the call site instead of a fresh object literal.
  const previewListStyle = useMemo(
    () => ({
      height: Math.min(preview.length * PREVIEW_ROW_HEIGHT, PREVIEW_LIST_MAX_HEIGHT),
    }),
    [preview.length]
  );

  // Memoize `rowProps` for the virtualized list (Round 1 F2.N-R1-31).
  // A fresh `{}` per render forced react-window's internal memo to bust
  // every state change, costing a full PreviewRow re-render across every
  // visible row for unrelated state updates (overflow toggles, drag
  // active, etc.).
  const previewRowProps = useMemo<PreviewRowData>(
    () => ({
      preview,
      formatTime: formatDisplayTime,
      origLabel: t("col_original"),
      shiftedLabel: t("col_shifted"),
    }),
    [preview, t]
  );

  // Live preview is computed from the FIRST file only. The same offset
  // applies uniformly to every file in a batch, so a sample of file #1
  // is honest; we don't re-render the timeline as the loop runs.
  useEffect(() => {
    if (!firstFileContent) {
      setPreview([]);
      setCaptionCount(0);
      setDetectedFormat("");
      return;
    }
    if (thresholdInvalid) {
      if (debounceRef.current) clearTimeout(debounceRef.current);
      setPreview([]);
      return;
    }
    if (debounceRef.current) clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(() => {
      try {
        const result = shiftSubtitles(firstFileContent, {
          offsetMs: effectiveOffsetMs,
          thresholdMs: thresholdMs ?? undefined,
        });
        setPreview(result.preview);
        setCaptionCount(result.captionCount);
        setDetectedFormat(result.format.toUpperCase());
      } catch {
        // Preview update failed — don't crash, just skip
      }
    }, 200);
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
  }, [firstFileContent, effectiveOffsetMs, thresholdMs, thresholdInvalid]);

  // Reset last-save outcome on selection change so "done" / "cancelled"
  // don't linger after the user picks a brand-new batch.
  useEffect(() => {
    setLastActionResult(null);
  }, [timingFiles]);

  // File-list dropdown: close on click-outside / Escape (mirrors HDR).
  useClickOutside(showFileList, fileContainerRef, () => setShowFileList(false));

  // Footer status. Busy carries the N-of-M progress; cancelled is its
  // own visible footer state, separate from done/error.
  const tabStatus = useMemo<Status>(() => {
    if (fileCount === 0) return { kind: "idle", message: t("status_timing_idle") };
    if (busy) {
      return {
        kind: "busy",
        message: t("status_timing_busy"),
        progress: progress ?? undefined,
      };
    }
    if (lastActionResult === "success") return { kind: "done", message: t("status_timing_done") };
    if (lastActionResult === "error") return { kind: "error", message: t("status_timing_error") };
    if (lastActionResult === "cancelled") {
      return { kind: "pending", message: t("status_timing_cancelled") };
    }
    return { kind: "pending", message: t("status_timing_pending") };
  }, [fileCount, busy, lastActionResult, progress, t]);
  useTabStatus("timing", tabStatus);

  // Shared ingestion path: conflict check → load first file's body for
  // the live preview → publish to context. The rest of the batch is
  // read on demand during the save loop, so memory stays flat even
  // for a 24-episode drop. Strict cross-tab dedup contract: any
  // conflict rejects the WHOLE selection — see buildConflictMessage /
  // FileContext for the rationale.
  const ingestPaths = useCallback(
    async (paths: string[], gen: number) => {
      const conflictMsg = buildConflictMessage(paths, "timing", isFileInUse, t);
      if (conflictMsg) {
        setDropError(conflictMsg);
        return;
      }
      setDropError(null);

      let firstContent: string;
      try {
        firstContent = await readText(paths[0]);
      } catch (e) {
        // Stale-pick guard BEFORE the log emit (N-R5-FEFEAT-06): an
        // earlier pick that errors after the user has already moved on
        // would otherwise emit a confusing log line tied to the
        // abandoned selection. Drop silently when superseded.
        if (gen !== pickGenRef.current) return;
        addLog(t("error_prefix", sanitizeError(e)), "error");
        return;
      }
      if (gen !== pickGenRef.current) return;

      const names = paths.map(fileNameFromPath);
      setTimingFiles({
        filePaths: paths,
        fileNames: names,
        firstFileContent: firstContent,
      });
    },
    [isFileInUse, setTimingFiles, addLog, t]
  );

  const handlePickFiles = useCallback(async () => {
    const gen = (pickGenRef.current = pickGenRef.current + 1);
    const paths = await pickSubtitleFiles(t);
    if (gen !== pickGenRef.current) return;
    if (!paths || paths.length === 0) return;
    await ingestPaths(paths, gen);
  }, [ingestPaths, t]);

  const handleDroppedPaths = useCallback(
    async (paths: string[]) => {
      const subtitlePaths = paths.filter((p) => fileNameHasSubtitleExt(fileNameFromPath(p)));
      if (subtitlePaths.length === 0) {
        // Surface through both the log AND the standard DropErrorBanner
        // (N-R5-FEFEAT-09). Users with collapsed log panels see nothing
        // from log-only — the banner is the always-visible feedback.
        const msg = t("msg_no_subtitle_in_drop");
        addLog(msg, "error");
        setDropError(msg);
        return;
      }
      const gen = (pickGenRef.current = pickGenRef.current + 1);
      await ingestPaths(subtitlePaths, gen);
    },
    [ingestPaths, addLog, t]
  );

  useFolderDrop({
    ref: dropZoneRef,
    onPaths: handleDroppedPaths,
    onActiveChange: setDropActive,
    onError: (e) => setDropError(sanitizeError(e)),
    disabled: busy,
    t,
  });

  const handleClearFiles = useCallback(() => {
    pickGenRef.current = pickGenRef.current + 1;
    clearFile("timing");
    setPreview([]);
    setCaptionCount(0);
    setDetectedFormat("");
    setDropError(null);
  }, [clearFile]);

  const handleSaveAll = useCallback(async () => {
    if (fileCount === 0 || thresholdInvalid) return;
    // Synchronous double-click gate — see HdrConvert::handleConvert
    // for the same idiom. Released in the outer finally below.
    if (busyRef.current) return;
    busyRef.current = true;
    try {
      const paths = filePaths;

      // Pre-flight overwrite check — same project-wide pattern as HDR
      // Convert. Template-derived output paths would silently overwrite
      // a previous run otherwise; one ask() before the batch is the
      // single safety net. Per-file try mirrors HDR Convert's shape
      // (Round 1 F2.N-R1-27): a single bad path used to fail the entire
      // batch with no attribution; now resolution failures fall through
      // to the main loop's per-file error logging and the pre-flight
      // existence check just skips them.
      const projectedOutputs: string[] = [];
      for (const filePath of paths) {
        try {
          projectedOutputs.push(deriveShiftedPath(filePath));
        } catch {
          // Re-thrown with per-file context inside the main loop below.
        }
      }
      try {
        const existingCount = await countExistingFiles(projectedOutputs);
        if (existingCount > 0) {
          const confirmed = await ask(t("msg_overwrite_confirm", existingCount, paths.length), {
            title: t("dialog_overwrite_title"),
            kind: "warning",
          });
          if (!confirmed) {
            addLog(t("msg_timing_cancelled"), "info");
            setLastActionResult("cancelled");
            return;
          }
        }
      } catch (e) {
        addLog(t("error_prefix", sanitizeError(e)), "error");
        setLastActionResult("error");
        return;
      }

      // Construct AbortController at the boundary into busy state — see
      // HdrConvert::handleConvert for rationale.
      abortRef.current = new AbortController();
      setBusy(true);
      setProgress({ processed: 0, total: paths.length });

      try {
        addLog(t("msg_timing_start", paths.length, effectiveOffsetMs));

        let successCount = 0;
        let processedCount = 0;
        const seenOutputs = new Set<string>();

        for (const filePath of paths) {
          if (abortRef.current?.signal.aborted) {
            addLog(t("msg_timing_cancelled"), "info");
            break;
          }

          // Defensive fallback: see HdrConvert's matching site —
          // `fileName` must remain bound to something usable across the
          // entire iteration so the catch block can attribute errors
          // even if a future `fileNameFromPath` refactor starts throwing
          // (Round 1 F2.N-R1-1).
          //
          // Wave 7.1 BiDi parity: same per-iteration sanitization as
          // HdrConvert / FontEmbed — every addLog interpolating fileName
          // gets BiDi-scrubbed display text from a single sanitize-at-
          // source.
          let fileName = sanitizeForDialog(filePath);
          try {
            fileName = sanitizeForDialog(fileNameFromPath(filePath));
          } catch {
            // Keep the raw path — better than no attribution.
          }
          addLog(t("msg_processing", fileName));

          try {
            const outputPath = deriveShiftedPath(filePath);

            // Within-batch dedup. Two inputs that resolve to the same
            // output path (different paths on disk pointing at the same
            // file via symlink, junction, etc.) would otherwise overwrite
            // each other. See normalizeOutputKey for the NFC + forward
            // slash + lowercase semantics.
            const normalizedOut = normalizeOutputKey(outputPath);
            if (seenOutputs.has(normalizedOut)) {
              addLog(t("msg_skipped_duplicate", fileName), "error");
              continue;
            }
            seenOutputs.add(normalizedOut);

            let content: string;
            try {
              content = await readText(filePath);
            } catch (e) {
              addLog(t("msg_read_error", fileName, sanitizeError(e)), "error");
              continue;
            }

            if (abortRef.current?.signal.aborted) break;

            const result = shiftSubtitles(content, {
              offsetMs: effectiveOffsetMs,
              thresholdMs: thresholdMs ?? undefined,
            });

            // Round 11 W11.1 (N1-R11-01): surface oversized-text drop
            // count to close the no-silent-action gap (parity with
            // HdrConvert's R10 N-R10-032 path). All four parsers push
            // skipped placeholders for entries exceeding
            // MAX_CAPTION_TEXT_LEN; builders filter them out so disk
            // output stays clean, but without this log the user has no
            // signal that input was partially dropped.
            if (result.skippedCount > 0) {
              addLog(t("msg_oversized_skipped", result.skippedCount, fileName), "warn");
            }

            if (abortRef.current?.signal.aborted) break;

            await writeText(outputPath, result.content);
            const outName = fileNameFromPath(outputPath);
            addLog(t("msg_saved", outName, result.captionCount), "success");
            successCount++;
          } catch (e) {
            addLog(t("msg_timing_error", fileName, sanitizeError(e)), "error");
          } finally {
            processedCount++;
            setProgress({ processed: processedCount, total: paths.length });
          }
        }

        // Cancel takes precedence over success/error — surfacing
        // "complete" when the user cancelled mid-batch would lie.
        // Avoid success-log vs error-footer contradiction
        // (N-R5-FEFEAT-17): when every file failed, the previous form
        // fired both "complete: 0/N" + red "failed" footer. Split so
        // success-log fires only when at least one file landed.
        const aborted = !!abortRef.current?.signal.aborted;
        if (aborted) {
          setLastActionResult("cancelled");
        } else if (successCount > 0) {
          addLog(t("msg_timing_complete", successCount, paths.length), "success");
          setLastActionResult("success");
        } else {
          addLog(t("msg_timing_all_failed", paths.length), "error");
          setLastActionResult("error");
        }
      } finally {
        setBusy(false);
        setProgress(null);
      }
    } finally {
      busyRef.current = false;
    }
  }, [fileCount, filePaths, effectiveOffsetMs, thresholdMs, thresholdInvalid, addLog, t]);

  const saveDisabled = fileCount === 0 || thresholdInvalid || busy;
  const saveLabel = fileCount > 1 ? t("btn_save_all", fileCount) : t("btn_save");

  return (
    <div className="space-y-4">
      {/* File strip — drop zone + filename(s) + clear + Select + Cancel + Save */}
      <div
        ref={dropZoneRef}
        className={`drop-zone flex items-center gap-2${dropActive ? " drop-active" : ""}`}
      >
        <div ref={fileContainerRef} className="flex-1 min-w-0" style={{ position: "relative" }}>
          {fileCount > 1 ? (
            <button
              type="button"
              onClick={() => setShowFileList((v) => !v)}
              className="w-full flex items-center gap-2 px-3 rounded-lg text-sm"
              style={{
                background: "var(--bg-panel)",
                border: "1px solid var(--border-light)",
                minHeight: "38px",
                color: "var(--text-primary)",
                textAlign: "left",
                cursor: "pointer",
              }}
              aria-expanded={showFileList}
              aria-haspopup="listbox"
            >
              <span className="truncate flex-1">{fileNames.join(", ")}</span>
              {detectedFormat && (
                <span
                  className="flex-none px-2 py-0.5 rounded text-xs"
                  style={{
                    background: "var(--bg-input)",
                    color: "var(--text-muted)",
                  }}
                >
                  {detectedFormat}
                </span>
              )}
              <span className="flex-none text-xs" style={{ color: "var(--text-muted)" }}>
                ({fileCount})
              </span>
              <span className="flex-none text-xs" style={{ color: "var(--text-muted)" }}>
                {showFileList ? "▲" : "▼"}
              </span>
            </button>
          ) : (
            <div
              className="flex items-center gap-2 px-3 rounded-lg text-sm"
              style={{
                background: fileCount > 0 ? "var(--bg-panel)" : "var(--bg-input)",
                border: "1px solid var(--border-light)",
                minHeight: "38px",
              }}
            >
              {fileCount > 0 ? (
                <>
                  <span className="truncate flex-1" style={{ color: "var(--text-primary)" }}>
                    {primaryFileName}
                  </span>
                  {detectedFormat && (
                    <span
                      className="flex-none px-2 py-0.5 rounded text-xs"
                      style={{
                        background: "var(--bg-input)",
                        color: "var(--text-muted)",
                      }}
                    >
                      {detectedFormat}
                    </span>
                  )}
                  {captionCount > 0 && (
                    <span className="flex-none text-xs" style={{ color: "var(--text-muted)" }}>
                      {t("captions_count", captionCount)}
                    </span>
                  )}
                </>
              ) : (
                <span className="italic" style={{ color: "var(--text-muted)" }}>
                  {t("file_empty")}
                </span>
              )}
            </div>
          )}

          {showFileList && fileCount > 1 && (
            <div
              className="absolute rounded-lg overflow-hidden flex flex-col"
              style={{
                top: "100%",
                left: 0,
                right: 0,
                marginTop: "4px",
                background: "var(--bg-panel)",
                border: "1px solid var(--border)",
                boxShadow: "var(--shadow-popover)",
                maxHeight: "190px",
                zIndex: 20,
              }}
              role="listbox"
            >
              <div
                className="px-3 py-2 flex-none"
                style={{ borderBottom: "1px solid var(--border)" }}
              >
                <span className="text-xs font-medium" style={{ color: "var(--text-muted)" }}>
                  {t("files_selected_title", fileCount)}
                </span>
              </div>
              <div className="overflow-y-auto flex-1">
                {fileNames.map((name, idx) => (
                  <div
                    key={idx}
                    className="px-3 py-2 text-sm truncate"
                    style={{
                      color: "var(--text-primary)",
                      borderBottom:
                        idx < fileNames.length - 1
                          ? "1px solid color-mix(in srgb, var(--border) 50%, transparent)"
                          : "none",
                    }}
                    title={filePaths[idx]}
                  >
                    {name}
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
        {fileCount > 0 && (
          <button
            onClick={handleClearFiles}
            disabled={busy}
            className="flex-none px-3 rounded-lg text-lg font-bold transition-colors"
            style={{
              background: busy ? "var(--bg-input)" : "var(--cancel-bg)",
              color: busy ? "var(--text-muted)" : "var(--cancel-text)",
              height: "38px",
            }}
            title={t("btn_clear_file")}
          >
            ✕
          </button>
        )}
        <button
          onClick={handlePickFiles}
          disabled={busy}
          className="flex-none px-5 rounded-lg font-medium text-sm transition-colors"
          style={{
            background: busy ? "var(--bg-input)" : "var(--accent)",
            color: busy ? "var(--text-muted)" : "white",
            height: "38px",
          }}
        >
          {t("btn_select_files")}
        </button>
        {busy && (
          <button
            onClick={() => {
              abortRef.current?.abort();
            }}
            className="flex-none px-4 rounded-lg text-sm transition-colors"
            style={{
              background: "var(--cancel-bg)",
              color: "var(--cancel-text)",
              height: "38px",
            }}
          >
            {t("btn_cancel")}
          </button>
        )}
        <button
          onClick={handleSaveAll}
          disabled={saveDisabled}
          className="flex-none px-6 rounded-lg font-medium text-sm transition-colors"
          style={{
            background: saveDisabled ? "var(--bg-input)" : "var(--accent)",
            color: saveDisabled ? "var(--text-muted)" : "white",
            height: "38px",
            minWidth: "120px",
          }}
        >
          {saveLabel}
        </button>
      </div>

      {/* Selection-rejected banner — same UX as HdrConvert. */}
      <DropErrorBanner message={dropError} onDismiss={() => setDropError(null)} />

      {/* Drop-zone discoverability hint — visible only when idle. */}
      {fileCount === 0 && !dropError && (
        <p className="text-xs ml-1" style={{ color: "var(--text-muted)" }}>
          {t("timing_drop_hint")}
        </p>
      )}

      {/* Offset value + unit */}
      <div className="flex items-end gap-3">
        <div>
          <label
            htmlFor="timing-offset-input"
            className="block text-sm font-medium mb-1"
            style={{ color: "var(--text-primary)" }}
          >
            {t("offset_label")}
          </label>
          {/* Round 8 Wave 8.6 — closes N-R5-FEFEAT-24 by migrating to the
              shared NumberInput. The string-shadow refactor (offsetText)
              is in place at the top of this component, so this matches
              HdrConvert's clear-then-retype semantics and inherits the
              invalid-border feedback for out-of-range typing instead of
              silently clamping in `effectiveOffsetMs`. */}
          <NumberInput
            id="timing-offset-input"
            value={offsetText}
            onChange={handleOffsetChange}
            min={-offsetMax}
            max={offsetMax}
            disabled={busy}
            invalid={offsetOutOfRange}
            className="w-28"
          />
        </div>
        <select
          id="timing-unit-select"
          name="offset-unit"
          aria-label={t("offset_label")}
          value={unit}
          onChange={(e) => setUnit(e.target.value as Unit)}
          disabled={busy}
          className="px-3 py-2 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
          style={{
            background: "var(--bg-input)",
            border: "1px solid var(--border)",
            color: "var(--text-primary)",
          }}
        >
          <option value="ms">{t("unit_ms")}</option>
          <option value="s">{t("unit_seconds")}</option>
        </select>
      </div>

      {/* Direction picker — two big buttons with arrow glyphs */}
      <div className="dir-picker" role="radiogroup" aria-label={t("offset_label")}>
        {/* Round 6 Wave 6.5 #23: role="radio" elements use aria-checked
            per the ARIA spec; aria-pressed is for role="button" toggles.
            Carrying both confused screen readers (some read "pressed"
            and "checked" twice for the same button); dropping
            aria-pressed leaves the spec-canonical attribute alone. */}
        <button
          type="button"
          className="dir-btn"
          role="radio"
          aria-checked={direction === "faster"}
          onClick={() => setDirection("faster")}
          disabled={busy}
        >
          <span className="dir-arrow" aria-hidden="true">
            ←
          </span>
          <span className="dir-label">{t("direction_faster")}</span>
        </button>
        <button
          type="button"
          className="dir-btn"
          role="radio"
          aria-checked={direction === "slower"}
          onClick={() => setDirection("slower")}
          disabled={busy}
        >
          <span className="dir-label">{t("direction_slower")}</span>
          <span className="dir-arrow" aria-hidden="true">
            →
          </span>
        </button>
      </div>

      {/* The -0.25rem / -0.5rem negative marginTop pulls these hint
          lines tighter under their controls (N-R5-FEFEAT-28). Negative
          margins are usually a footgun; pinned here as deliberate
          density tuning, not an oversight. Switch to a parent `gap`
          adjustment if the surrounding flex container gains explicit
          gap control later. */}
      <p className="text-xs" style={{ color: "var(--text-muted)", marginTop: "-0.25rem" }}>
        {t("offset_hint")}
      </p>

      {/* Threshold */}
      <div className="flex items-center gap-3">
        <label
          className="flex items-center gap-2 text-sm cursor-pointer"
          style={{ color: "var(--text-primary)" }}
        >
          <input
            type="checkbox"
            id="timing-threshold-checkbox"
            name="threshold-enabled"
            checked={useThreshold}
            onChange={(e) => setUseThreshold(e.target.checked)}
            disabled={busy}
            className="rounded"
            style={{
              background: "var(--bg-input)",
              borderColor: "var(--border)",
            }}
          />
          {t("threshold_label")}
        </label>
        {useThreshold && (
          <input
            type="text"
            id="timing-threshold-input"
            name="threshold"
            value={thresholdText}
            onChange={(e) => setThresholdText(e.target.value)}
            disabled={busy}
            placeholder="00:05:00.000"
            className="w-40 px-3 py-1.5 rounded-lg text-sm font-mono focus:outline-none focus:ring-2 focus:ring-blue-500"
            style={{
              background: "var(--bg-input)",
              border: "1px solid var(--border)",
              color: "var(--text-primary)",
            }}
          />
        )}
        {thresholdInvalid && (
          <span className="text-xs" style={{ color: "var(--error)" }}>
            {t("threshold_invalid")}
          </span>
        )}
        {!thresholdInvalid && thresholdExceedsFile && (
          // Round 7 Wave 7.6 (N4-R7-6): threshold-exceeds-file is a
          // warning (the shift will still run, but the threshold gate
          // is moot), not a save-blocker — use --warning, not --error.
          // thresholdInvalid above stays at --error because that path
          // genuinely blocks the run (NaN / negative threshold).
          <span className="text-xs" style={{ color: "var(--warning)" }}>
            {t("threshold_exceeds_file")}
          </span>
        )}
      </div>
      {useThreshold && (
        <p
          className="text-xs"
          style={{
            color: "var(--text-muted)",
            marginTop: "-0.5rem",
            // ZH version of this hint is ~3x the English byte length and
            // can overflow narrow viewports; allow wrapping (rather than
            // splitting the i18n key or shortening the ZH copy) so the
            // English layout stays compact while the ZH wraps gracefully.
            whiteSpace: "normal",
            overflowWrap: "anywhere",
          }}
        >
          {t("threshold_format_hint")}
        </p>
      )}

      {/* Timeline preview — shows the FIRST file's captions in batch mode
          (the same offset applies uniformly, so file #1 is a representative
          sample). Frozen header sits outside the scroll area.
          aria-live polite: SR users hear when the preview recomputes after
          offset / unit / direction / threshold changes; "polite" defers to
          the user's current speech rather than interrupting. */}
      {preview.length > 0 && (
        <div className="timeline-preview" aria-live="polite">
          <div className="timeline-preview-head">
            <span>
              {fileCount > 1
                ? t("preview_title_first", preview.length, primaryFileName)
                : t("preview_title", preview.length)}
            </span>
          </div>
          <div className="timeline-row timeline-row-header" aria-hidden="true">
            <span>{t("col_index")}</span>
            <span>{t("col_original")}</span>
            <span>{t("col_shifted")}</span>
            <span>{t("col_text")}</span>
          </div>
          <List<PreviewRowData>
            className="timeline-list"
            // previewListStyle is a useMemo identifier — passes the inline-style
            // CSS-injection lint, which forbids object literals / call
            // expressions at the JSX call site (those can hide user-controlled
            // input). The computed value comes from preview.length only.
            style={previewListStyle}
            rowCount={preview.length}
            rowHeight={PREVIEW_ROW_HEIGHT}
            rowComponent={PreviewRow}
            rowProps={previewRowProps}
          />
        </div>
      )}

      {/* Log */}
      <LogPanel logs={logs} onClear={clearLogs} scrollRef={logScrollRef} />
    </div>
  );
}

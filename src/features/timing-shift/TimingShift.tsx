import { useState, useCallback, useEffect, useRef, useMemo } from "react";
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
import { TAB_LABEL_KEYS } from "../../lib/tab-labels";
import type { TabId } from "../../lib/FileContext";

type Unit = "ms" | "s";
type Direction = "slower" | "faster";

/** Cap to ±1 year to prevent integer precision loss for extreme inputs. */
const MAX_OFFSET_MS = 365 * 24 * 3600 * 1000;

interface LogEntry {
  id: number;
  text: string;
  type: "info" | "error" | "success";
}

// Subtitle extensions Time Shift accepts. Used by the folder-drop filter
// to keep videos and unrelated files out of the batch when a user drops
// a whole show folder.
const SUBTITLE_EXTS = new Set(["ass", "ssa", "srt", "vtt", "sub", "sbv", "lrc"]);

function fileNameHasSubtitleExt(name: string): boolean {
  const dot = name.lastIndexOf(".");
  if (dot < 0) return false;
  return SUBTITLE_EXTS.has(name.slice(dot + 1).toLowerCase());
}

export default function TimingShift() {
  const { t } = useI18n();
  const { timingFiles, setTimingFiles, clearFile, isFileInUse } = useFileContext();

  const [detectedFormat, setDetectedFormat] = useState<string>("");
  const [offsetValue, setOffsetValue] = useState(200);
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
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [showFileList, setShowFileList] = useState(false);
  // Selection-rejection banner — see HdrConvert for the full rationale.
  // Strict cross-tab dedup: any conflict in the new selection rejects
  // the entire drop, leaving the prior state untouched.
  const [dropError, setDropError] = useState<string | null>(null);

  const debounceRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const pickGenRef = useRef(0);
  const cancelRef = useRef(false);
  const logIdRef = useRef(0);
  const dropZoneRef = useRef<HTMLDivElement>(null);
  // Scroll container for the log — see HdrConvert for the rationale
  // behind avoiding scrollIntoView (it walks ancestors and can scroll
  // .window past the titlebar in Chromium).
  const logScrollRef = useRef<HTMLDivElement>(null);
  const fileContainerRef = useRef<HTMLDivElement>(null);

  const effectiveOffsetMs = useMemo(() => {
    const base = unit === "s" ? offsetValue * 1000 : offsetValue;
    const clamped = Math.max(-MAX_OFFSET_MS, Math.min(MAX_OFFSET_MS, base));
    return direction === "faster" ? -clamped : clamped;
  }, [unit, offsetValue, direction]);

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
  useEffect(() => {
    if (!showFileList) return;
    const onClick = (e: MouseEvent) => {
      if (fileContainerRef.current && !fileContainerRef.current.contains(e.target as Node)) {
        setShowFileList(false);
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setShowFileList(false);
    };
    const id = setTimeout(() => document.addEventListener("mousedown", onClick), 0);
    document.addEventListener("keydown", onKey);
    return () => {
      clearTimeout(id);
      document.removeEventListener("mousedown", onClick);
      document.removeEventListener("keydown", onKey);
    };
  }, [showFileList]);

  const addLog = useCallback((text: string, type: LogEntry["type"] = "info") => {
    const id = logIdRef.current++;
    setLogs((prev) => {
      const next = [...prev, { id, text, type }];
      return next.length > 200 ? next.slice(-200) : next;
    });
    setTimeout(() => {
      const el = logScrollRef.current;
      if (el) el.scrollTop = el.scrollHeight;
    }, 50);
  }, []);

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

  // Strict cross-tab dedup. If any path is loaded in another tab, the
  // whole selection is rejected — the user gets a visible banner naming
  // the conflicting tab and the count, and timingFiles stays untouched.
  // Same rationale as HdrConvert.checkConflicts.
  const checkConflicts = useCallback(
    (paths: string[]): string | null => {
      let conflictCount = 0;
      let conflictTab: TabId | null = null;
      for (const p of paths) {
        const usedIn = isFileInUse(p, "timing");
        if (usedIn) {
          if (conflictTab === null) conflictTab = usedIn;
          conflictCount++;
        }
      }
      if (conflictTab === null) return null;
      return t("msg_dedup_blocked", conflictCount, t(TAB_LABEL_KEYS[conflictTab]));
    },
    [isFileInUse, t]
  );

  // Shared ingestion path: conflict check → load first file's body for
  // the live preview → publish to context. The rest of the batch is
  // read on demand during the save loop, so memory stays flat even
  // for a 24-episode drop.
  const ingestPaths = useCallback(
    async (paths: string[], gen: number) => {
      const conflictMsg = checkConflicts(paths);
      if (conflictMsg) {
        setDropError(conflictMsg);
        return;
      }
      setDropError(null);

      let firstContent: string;
      try {
        firstContent = await readText(paths[0]);
      } catch (e) {
        addLog(t("error_prefix", e instanceof Error ? e.message : String(e)), "error");
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
    [checkConflicts, setTimingFiles, addLog, t]
  );

  const handlePickFiles = useCallback(async () => {
    const gen = (pickGenRef.current = pickGenRef.current + 1);
    const paths = await pickSubtitleFiles();
    if (gen !== pickGenRef.current) return;
    if (!paths || paths.length === 0) return;
    await ingestPaths(paths, gen);
  }, [ingestPaths]);

  const handleDroppedPaths = useCallback(
    async (paths: string[]) => {
      const subtitlePaths = paths.filter((p) => fileNameHasSubtitleExt(fileNameFromPath(p)));
      if (subtitlePaths.length === 0) {
        addLog(t("msg_no_subtitle_in_drop"), "error");
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
    disabled: busy,
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

    const paths = filePaths;

    // Pre-flight overwrite check — same project-wide pattern as HDR
    // Convert. Template-derived output paths would silently overwrite
    // a previous run otherwise; one ask() before the batch is the
    // single safety net.
    const projectedOutputs = paths.map((p) => deriveShiftedPath(p));
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

    setBusy(true);
    setProgress({ processed: 0, total: paths.length });
    cancelRef.current = false;

    try {
      addLog(t("msg_timing_start", paths.length, effectiveOffsetMs));

      let successCount = 0;
      let processedCount = 0;
      const seenOutputs = new Set<string>();

      for (let i = 0; i < paths.length; i++) {
        const filePath = paths[i];

        if (cancelRef.current) {
          addLog(t("msg_timing_cancelled"), "info");
          break;
        }

        const fileName = fileNameFromPath(filePath);
        addLog(t("msg_processing", fileName));

        try {
          const outputPath = deriveShiftedPath(filePath);

          // Within-batch dedup. Two inputs that resolve to the same
          // output path (different paths on disk pointing at the same
          // file via symlink, junction, etc.) would otherwise overwrite
          // each other. NFC + forward-slash + lowercase normalization
          // matches HDR Convert's dedup semantics.
          const normalizedOut = outputPath.normalize("NFC").replace(/\\/g, "/").toLowerCase();
          if (seenOutputs.has(normalizedOut)) {
            addLog(t("msg_skipped_duplicate", fileName), "error");
            continue;
          }
          seenOutputs.add(normalizedOut);

          let content: string;
          try {
            content = await readText(filePath);
          } catch (e) {
            addLog(
              t("msg_read_error", fileName, e instanceof Error ? e.message : String(e)),
              "error"
            );
            continue;
          }

          if (cancelRef.current) break;

          const result = shiftSubtitles(content, {
            offsetMs: effectiveOffsetMs,
            thresholdMs: thresholdMs ?? undefined,
          });

          if (cancelRef.current) break;

          await writeText(outputPath, result.content);
          const outName = fileNameFromPath(outputPath);
          addLog(t("msg_saved", outName, result.captionCount), "success");
          successCount++;
        } catch (e) {
          addLog(
            t("msg_timing_error", fileName, e instanceof Error ? e.message : String(e)),
            "error"
          );
        } finally {
          processedCount++;
          setProgress({ processed: processedCount, total: paths.length });
        }
      }

      if (!cancelRef.current) {
        addLog(t("msg_timing_complete", successCount, paths.length), "success");
      }

      // Cancel takes precedence over success/error — surfacing
      // "complete" when the user cancelled mid-batch would lie.
      if (cancelRef.current) {
        setLastActionResult("cancelled");
      } else {
        setLastActionResult(successCount > 0 ? "success" : "error");
      }
    } finally {
      setBusy(false);
      setProgress(null);
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
              background: "var(--cancel-bg)",
              color: "var(--cancel-text)",
              opacity: busy ? 0.4 : 1,
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
              cancelRef.current = true;
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
            opacity: fileCount === 0 ? 0.5 : 1,
            height: "38px",
            minWidth: "120px",
          }}
        >
          {saveLabel}
        </button>
      </div>

      {/* Selection-rejected banner — same UX as HdrConvert. */}
      {dropError && (
        <div
          className="flex items-center justify-between gap-3 px-3 py-2 rounded-lg text-sm"
          role="alert"
          style={{
            background: "var(--cancel-bg)",
            border: "1px solid var(--error)",
            color: "var(--error)",
          }}
        >
          <span>{dropError}</span>
          <button
            type="button"
            onClick={() => setDropError(null)}
            aria-label={t("btn_clear_file")}
            className="flex-none text-base"
            style={{ color: "var(--error)", lineHeight: 1 }}
          >
            ✕
          </button>
        </div>
      )}

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
          <input
            id="timing-offset-input"
            name="offset"
            type="number"
            value={offsetValue}
            onChange={(e) => {
              const n = parseInt(e.target.value, 10);
              if (!Number.isNaN(n)) setOffsetValue(n);
            }}
            disabled={busy}
            className="w-28 px-3 py-2 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
            style={{
              background: "var(--bg-input)",
              border: "1px solid var(--border)",
              color: "var(--text-primary)",
            }}
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
        <button
          type="button"
          className="dir-btn"
          role="radio"
          aria-pressed={direction === "faster"}
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
          aria-pressed={direction === "slower"}
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
          <span className="text-xs" style={{ color: "var(--error)" }}>
            {t("threshold_exceeds_file")}
          </span>
        )}
      </div>
      {useThreshold && (
        <p className="text-xs" style={{ color: "var(--text-muted)", marginTop: "-0.5rem" }}>
          {t("threshold_format_hint")}
        </p>
      )}

      {/* Timeline preview — shows the FIRST file's captions in batch mode
          (the same offset applies uniformly, so file #1 is a representative
          sample). Frozen header sits outside the scroll area. */}
      {preview.length > 0 && (
        <div className="timeline-preview">
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
          <div className="timeline-list">
            {preview.map((entry) => (
              <div
                key={entry.index}
                className={"timeline-row" + (entry.wasShifted ? "" : " unchanged")}
              >
                <span className="idx">{entry.index}</span>
                <span
                  className="t-orig"
                  title={`${t("col_original")}: ${formatDisplayTime(entry.originalStart)}`}
                >
                  {formatDisplayTime(entry.originalStart)}
                </span>
                <span
                  className="t-new"
                  title={`${t("col_shifted")}: ${formatDisplayTime(entry.shiftedStart)}`}
                >
                  {formatDisplayTime(entry.shiftedStart)}
                </span>
                <span className="txt" title={entry.fullText}>
                  {entry.text}
                </span>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Log */}
      {logs.length > 0 && (
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
              onClick={() => setLogs([])}
              className="text-xs"
              style={{ color: "var(--text-muted)" }}
            >
              {t("log_clear")}
            </button>
          </div>
          <div
            ref={logScrollRef}
            className="max-h-48 overflow-y-auto p-3 font-mono text-xs space-y-0.5"
          >
            {logs.map((log) => (
              <div
                key={log.id}
                style={{
                  color: {
                    error: "var(--error)",
                    success: "var(--success)",
                    info: "var(--text-muted)",
                  }[log.type],
                }}
              >
                {log.text}
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

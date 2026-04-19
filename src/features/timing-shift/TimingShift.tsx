import { useState, useCallback, useEffect, useRef, useMemo } from "react";
import {
  pickSubtitleFile,
  pickSavePath,
  readText,
  writeText,
  fileNameFromPath,
} from "../../lib/tauri-api";
import {
  shiftSubtitles,
  formatDisplayTime,
  parseDisplayTime,
  type ShiftResult,
  type PreviewEntry,
} from "./timing-engine";
import { useI18n } from "../../i18n/useI18n";
import { useFileContext } from "../../lib/FileContext";
import { useStatus } from "../../lib/StatusContext";

type Unit = "ms" | "s";
type Direction = "slower" | "faster";

export default function TimingShift() {
  const { t } = useI18n();
  const { timingFile, setTimingFile, clearFile, isFileInUse } = useFileContext();

  const [detectedFormat, setDetectedFormat] = useState<string>("");

  const [offsetValue, setOffsetValue] = useState(200);
  const [unit, setUnit] = useState<Unit>("ms");
  const [direction, setDirection] = useState<Direction>("slower");

  const [useThreshold, setUseThreshold] = useState(false);
  const [thresholdText, setThresholdText] = useState("00:05:00.000");

  const [preview, setPreview] = useState<PreviewEntry[]>([]);
  const [captionCount, setCaptionCount] = useState(0);
  const [status, setStatus] = useState<string>("");
  const [isError, setIsError] = useState(false);
  const [busy, setBusy] = useState(false);
  const [lastActionResult, setLastActionResult] = useState<"success" | "error" | null>(null);
  // Rename the context's setStatus to avoid shadowing the local one above.
  const { setStatus: setTabStatus } = useStatus();

  const debounceRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const pickGenRef = useRef(0);

  // Memoized derived values — prevents debounce effect from resetting on unrelated state updates
  const effectiveOffsetMs = useMemo(() => {
    const base = unit === "s" ? offsetValue * 1000 : offsetValue;
    // Cap to ±1 year to prevent integer precision loss for extreme inputs
    const MAX_OFFSET_MS = 365 * 24 * 3600 * 1000;
    const clamped = Math.max(-MAX_OFFSET_MS, Math.min(MAX_OFFSET_MS, base));
    return direction === "faster" ? -clamped : clamped;
  }, [unit, offsetValue, direction]);

  // Returns number when valid, null when invalid or disabled.
  // Consistently null (not undefined) so all guards can use strict === null.
  const thresholdMs = useMemo(
    () => (useThreshold ? parseDisplayTime(thresholdText) : null),
    [useThreshold, thresholdText]
  );
  const thresholdInvalid = useThreshold && thresholdMs === null;

  // Last caption's START time — shiftSubtitle uses `c.start >= threshold` to
  // decide which captions move, so a threshold in the gap between the last
  // caption's start and end still produces zero shifts. Comparing against
  // maxCaptionStart rather than maxCaptionEnd makes the warning fire in that
  // gap window too, matching the actual shift semantics users observe.
  const maxCaptionStart = useMemo(
    () => preview.reduce((max, e) => Math.max(max, e.originalStart), 0),
    [preview]
  );
  const thresholdExceedsFile =
    useThreshold && thresholdMs !== null && maxCaptionStart > 0 && thresholdMs > maxCaptionStart;

  // Derive file state from context
  const filePath = timingFile?.filePath ?? null;
  const fileName = timingFile?.fileName ?? "";
  const fileContent = timingFile?.fileContent ?? "";

  // Update preview whenever parameters change (debounced to avoid reprocessing on every keystroke)
  useEffect(() => {
    if (!fileContent) return;
    if (thresholdInvalid) {
      if (debounceRef.current) clearTimeout(debounceRef.current);
      setPreview([]);
      return;
    }
    if (debounceRef.current) clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(() => {
      try {
        const result = shiftSubtitles(fileContent, {
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
  }, [fileContent, effectiveOffsetMs, thresholdMs, thresholdInvalid]);

  const handlePickFile = useCallback(async () => {
    const gen = (pickGenRef.current = pickGenRef.current + 1);

    const path = await pickSubtitleFile();
    if (gen !== pickGenRef.current) return;
    if (!path) return;

    // Cross-tab duplicate guard
    const usedIn = isFileInUse(path, "timing");
    if (usedIn) {
      setIsError(true);
      setStatus(t("msg_file_in_use", t("tab_" + usedIn)));
      return;
    }

    setStatus("");
    setIsError(false);

    try {
      clearFile("timing");
      const content = await readText(path);
      if (gen !== pickGenRef.current) return;
      const name = fileNameFromPath(path);

      const result = shiftSubtitles(content, {
        offsetMs: effectiveOffsetMs,
        thresholdMs: thresholdMs ?? undefined,
      });
      setPreview(result.preview);
      setCaptionCount(result.captionCount);
      setDetectedFormat(result.format.toUpperCase());

      // Silent replace: see FileContext.tsx for design rationale
      setTimingFile({
        filePath: path,
        fileName: name,
        fileContent: content,
      });
    } catch (e) {
      if (gen !== pickGenRef.current) return;
      setIsError(true);
      setStatus(t("error_prefix", e instanceof Error ? e.message : String(e)));
    }
  }, [effectiveOffsetMs, thresholdMs, isFileInUse, setTimingFile, clearFile, t]);

  const handleClearFile = useCallback(() => {
    pickGenRef.current = pickGenRef.current + 1;
    clearFile("timing");
    setPreview([]);
    setCaptionCount(0);
    setDetectedFormat("");
    setStatus("");
    setIsError(false);
  }, [clearFile]);

  const handleSave = useCallback(async () => {
    if (!fileContent || !filePath) return;
    if (thresholdInvalid) return;

    setBusy(true);
    try {
      // Always recompute from current parameters — do not cache. A cached result
      // can go stale if the user changes params and clicks Save within the 200ms
      // debounce window, producing output that doesn't match the UI settings.
      const result: ShiftResult = shiftSubtitles(fileContent, {
        offsetMs: effectiveOffsetMs,
        thresholdMs: thresholdMs ?? undefined,
      });

      // Suggest output filename
      const lastDot = fileName.lastIndexOf(".");
      const ext = lastDot > 0 ? fileName.slice(lastDot) : "";
      const baseName = lastDot > 0 ? fileName.slice(0, lastDot) : fileName;
      const defaultName = `${baseName}.shifted${ext}`;

      const savePath = await pickSavePath(defaultName);
      if (!savePath) {
        setBusy(false);
        return;
      }

      await writeText(savePath, result.content);
      const outName = fileNameFromPath(savePath);
      setIsError(false);
      setStatus(t("msg_saved", outName, result.captionCount));
      setLastActionResult("success");
    } catch (e) {
      setIsError(true);
      setStatus(t("error_prefix", e instanceof Error ? e.message : String(e)));
      setLastActionResult("error");
    } finally {
      setBusy(false);
    }
  }, [fileContent, filePath, fileName, effectiveOffsetMs, thresholdMs, thresholdInvalid, t]);

  // Reset last-save outcome on file change so "done" doesn't stick around.
  useEffect(() => {
    setLastActionResult(null);
  }, [timingFile]);

  // Publish status to the shared context — footer picks it up per active tab.
  useEffect(() => {
    if (!fileName) {
      setTabStatus("timing", { kind: "idle", message: t("status_timing_idle") });
      return;
    }
    if (busy) {
      setTabStatus("timing", { kind: "busy", message: t("status_timing_busy") });
      return;
    }
    if (lastActionResult === "success") {
      setTabStatus("timing", { kind: "done", message: t("status_timing_done") });
      return;
    }
    if (lastActionResult === "error") {
      setTabStatus("timing", { kind: "error", message: t("status_timing_error") });
      return;
    }
    setTabStatus("timing", { kind: "pending", message: t("status_timing_pending") });
  }, [fileName, busy, lastActionResult, setTabStatus, t]);

  return (
    <div className="space-y-4">
      {/* ── File strip — always visible; filename + badges + clear + Select ── */}
      <div className="flex items-center gap-2">
        <div
          className="flex-1 min-w-0 flex items-center gap-2 px-3 rounded-lg text-sm"
          style={{
            background: fileName ? "var(--bg-panel)" : "var(--bg-input)",
            border: "1px solid var(--border-light)",
            minHeight: "38px",
          }}
        >
          {fileName ? (
            <>
              <span className="truncate flex-1" style={{ color: "var(--text-primary)" }}>
                {fileName}
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
        {fileName && (
          <button
            onClick={handleClearFile}
            className="flex-none px-3 rounded-lg text-lg font-bold transition-colors"
            style={{
              background: "var(--cancel-bg)",
              color: "var(--cancel-text)",
              height: "38px",
            }}
            title={t("btn_clear_file")}
          >
            ✕
          </button>
        )}
        <button
          onClick={handlePickFile}
          disabled={busy}
          className="flex-none px-5 rounded-lg font-medium text-sm transition-colors"
          style={{
            background: busy ? "var(--bg-input)" : "var(--accent)",
            color: busy ? "var(--text-muted)" : "white",
            height: "38px",
          }}
        >
          {t("btn_select_file")}
        </button>
        <button
          onClick={handleSave}
          disabled={!filePath || thresholdInvalid}
          className="flex-none px-6 rounded-lg font-medium text-sm transition-colors"
          style={{
            background: !filePath || thresholdInvalid ? "var(--bg-input)" : "var(--accent)",
            color: !filePath || thresholdInvalid ? "var(--text-muted)" : "white",
            opacity: !filePath ? 0.5 : 1,
            height: "38px",
            minWidth: "120px",
          }}
        >
          {t("btn_save_as")}
        </button>
      </div>

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

      {/* Timeline preview — full caption list with struck-through originals.
          Column header row sits outside the scroll area so it stays visible
          (Excel-style frozen header). */}
      {preview.length > 0 && (
        <div className="timeline-preview">
          <div className="timeline-preview-head">
            <span>{t("preview_title", preview.length)}</span>
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

      {/* Status */}
      {status && (
        <p
          className="text-sm"
          style={{
            color: isError ? "var(--error)" : "var(--success)",
          }}
        >
          {status}
        </p>
      )}
    </div>
  );
}

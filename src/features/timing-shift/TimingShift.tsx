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
  formatTimestamp,
  parseTimestamp,
  type ShiftResult,
  type PreviewEntry,
} from "./timing-engine";
import { useI18n } from "../../i18n/useI18n";
import { useFileContext } from "../../lib/FileContext";

type Unit = "ms" | "s";
type Direction = "slower" | "faster";

export default function TimingShift() {
  const { t } = useI18n();
  const { timingFile, setTimingFile, clearFile, isFileInUse } = useFileContext();

  const [detectedFormat, setDetectedFormat] = useState<string>("");

  const [offsetValue, setOffsetValue] = useState(2000);
  const [unit, setUnit] = useState<Unit>("ms");
  const [direction, setDirection] = useState<Direction>("slower");

  const [useThreshold, setUseThreshold] = useState(false);
  const [thresholdText, setThresholdText] = useState("00:05:00.000");

  const [preview, setPreview] = useState<PreviewEntry[]>([]);
  const [captionCount, setCaptionCount] = useState(0);
  const [status, setStatus] = useState<string>("");
  const [isError, setIsError] = useState(false);

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
    () => (useThreshold ? parseTimestamp(thresholdText) : null),
    [useThreshold, thresholdText]
  );

  // Derive file state from context
  const filePath = timingFile?.filePath ?? null;
  const fileName = timingFile?.fileName ?? "";
  const fileContent = timingFile?.fileContent ?? "";

  // Update preview whenever parameters change (debounced to avoid reprocessing on every keystroke)
  useEffect(() => {
    if (!fileContent) return;
    if (useThreshold && thresholdMs === null) {
      if (debounceRef.current) clearTimeout(debounceRef.current);
      // eslint-disable-next-line react-hooks/set-state-in-effect -- guard clause: clears stale preview, cannot cascade
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
  }, [fileContent, effectiveOffsetMs, thresholdMs, useThreshold]);

  const handlePickFile = useCallback(async () => {
    const gen = pickGenRef.current = pickGenRef.current + 1;

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
    if (useThreshold && thresholdMs === null) return;

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
      if (!savePath) return;

      await writeText(savePath, result.content);
      const outName = fileNameFromPath(savePath);
      setIsError(false);
      setStatus(t("msg_saved", outName, result.captionCount));
    } catch (e) {
      setIsError(true);
      setStatus(t("error_prefix", e instanceof Error ? e.message : String(e)));
    }
  }, [fileContent, filePath, fileName, effectiveOffsetMs, thresholdMs, useThreshold, t]);

  return (
    <div className="space-y-5">
      {/* ── Top area: file info left + buttons right ── */}
      <div className="flex items-start justify-between gap-6">
        {/* Left: file name + format badge */}
        <div className="flex items-center gap-3 flex-1 min-w-0">
          {fileName && (
            <>
              <span
                className="text-sm truncate"
                style={{ color: "var(--text-primary)" }}
              >
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
              <button
                onClick={handleClearFile}
                className="flex-none px-3 py-2 rounded-lg text-lg font-bold transition-colors"
                style={{
                  background: "var(--cancel-bg)",
                  color: "var(--cancel-text)",
                }}
                title={t("btn_clear_file")}
              >
                ✕
              </button>
            </>
          )}
        </div>

        {/* Right: stacked action buttons */}
        <div className="flex flex-col gap-2 flex-none" style={{ minWidth: "130px" }}>
          <button
            onClick={handlePickFile}
            className="w-full px-5 py-2.5 rounded-lg font-medium text-sm transition-colors"
            style={{
              background: "var(--accent)",
              color: "white",
            }}
          >
            {t("btn_select_file")}
          </button>
          <button
            onClick={handleSave}
            disabled={!filePath || (useThreshold && thresholdMs === null)}
            className="w-full px-5 py-2.5 rounded-lg font-medium text-sm transition-colors"
            style={{
              background: !filePath || (useThreshold && thresholdMs === null)
                ? "var(--bg-input)"
                : "var(--accent)",
              color: !filePath || (useThreshold && thresholdMs === null)
                ? "var(--text-muted)"
                : "white",
              opacity: !filePath ? 0.5 : 1,
            }}
          >
            {t("btn_save_as")}
          </button>
        </div>
      </div>

      {/* Offset Controls */}
      <div className="flex items-end gap-3">
        <div>
          <label
            className="block text-sm font-medium mb-1"
            style={{ color: "var(--text-primary)" }}
          >
            {t("offset_label")}
          </label>
          <input
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
        <select
          value={direction}
          onChange={(e) => setDirection(e.target.value as Direction)}
          className="px-3 py-2 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
          style={{
            background: "var(--bg-input)",
            border: "1px solid var(--border)",
            color: "var(--text-primary)",
          }}
        >
          <option value="slower">{t("direction_slower")}</option>
          <option value="faster">{t("direction_faster")}</option>
        </select>
      </div>
      <p className="text-xs" style={{ color: "var(--text-muted)", marginTop: "-0.5rem" }}>
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
        {useThreshold && thresholdMs === null && (
          <span className="text-xs" style={{ color: "var(--error)" }}>
            {t("threshold_invalid")}
          </span>
        )}
      </div>

      {/* Preview */}
      {preview.length > 0 && (
        <div
          className="rounded-lg"
          style={{
            border: "1px solid var(--border)",
            background: "var(--bg-panel)",
          }}
        >
          <div
            className="px-3 py-2"
            style={{ borderBottom: "1px solid var(--border)" }}
          >
            <span
              className="text-xs font-medium"
              style={{ color: "var(--text-muted)" }}
            >
              {t("preview_title", preview.length, captionCount)}
            </span>
          </div>
          <div className="max-h-64 overflow-y-auto">
            <table className="w-full text-xs font-mono">
              <thead>
                <tr style={{ borderBottom: "1px solid var(--border)" }}>
                  <th
                    className="px-3 py-1.5 text-left"
                    style={{ color: "var(--text-muted)" }}
                  >
                    {t("col_index")}
                  </th>
                  <th
                    className="px-3 py-1.5 text-left"
                    style={{ color: "var(--text-muted)" }}
                  >
                    {t("col_original")}
                  </th>
                  <th
                    className="px-3 py-1.5 text-center"
                    style={{ color: "var(--text-muted)" }}
                  >
                    →
                  </th>
                  <th
                    className="px-3 py-1.5 text-left"
                    style={{ color: "var(--text-muted)" }}
                  >
                    {t("col_shifted")}
                  </th>
                </tr>
              </thead>
              <tbody>
                {preview.map((entry) => (
                  <tr
                    key={entry.index}
                    className={entry.wasShifted ? "" : "opacity-50"}
                    style={{
                      borderBottom: "1px solid color-mix(in srgb, var(--border) 50%, transparent)",
                    }}
                  >
                    <td
                      className="px-3 py-1"
                      style={{ color: "var(--text-muted)" }}
                    >
                      {entry.index}
                    </td>
                    <td
                      className="px-3 py-1"
                      style={{ color: "var(--text-muted)" }}
                    >
                      {formatTimestamp(entry.originalStart)}
                    </td>
                    <td
                      className="px-3 py-1 text-center"
                      style={{ color: "var(--text-muted)" }}
                    >
                      {entry.wasShifted ? "→" : "·"}
                    </td>
                    <td
                      className="px-3 py-1"
                      style={{
                        color: entry.wasShifted
                          ? "var(--preview-shifted)"
                          : "var(--text-muted)",
                      }}
                    >
                      {formatTimestamp(entry.shiftedStart)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}

      {/* Status */}
      {status && (
        <p
          className="text-sm"
          style={{
            color: isError
              ? "var(--error)"
              : "var(--success)",
          }}
        >
          {status}
        </p>
      )}
    </div>
  );
}

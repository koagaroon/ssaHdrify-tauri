import { useState, useCallback, useEffect, useRef } from "react";
import {
  pickSubtitleFile,
  pickSavePath,
  readText,
  writeText,
} from "../../lib/tauri-api";
import {
  shiftSubtitles,
  formatTimestamp,
  parseTimestamp,
  type ShiftResult,
  type PreviewEntry,
} from "./timing-engine";
import { useI18n } from "../../i18n/useI18n";

type Unit = "ms" | "s";
type Direction = "slower" | "faster";

export default function TimingShift() {
  const { t } = useI18n();

  const [filePath, setFilePath] = useState<string | null>(null);
  const [fileName, setFileName] = useState<string>("");
  const [fileContent, setFileContent] = useState<string>("");
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

  const debounceRef = useRef<ReturnType<typeof setTimeout>>();


  // Compute effective offset in ms
  const effectiveOffsetMs = (() => {
    const base = unit === "s" ? offsetValue * 1000 : offsetValue;
    return direction === "faster" ? -base : base;
  })();

  // Compute threshold in ms
  const thresholdMs = useThreshold ? parseTimestamp(thresholdText) : undefined;

  // Update preview whenever parameters change (debounced to avoid reprocessing on every keystroke)
  useEffect(() => {
    if (!fileContent) return;
    if (useThreshold && thresholdMs == null) {
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
  }, [fileContent, effectiveOffsetMs, thresholdMs, useThreshold]);

  const handlePickFile = useCallback(async () => {
    const path = await pickSubtitleFile();
    if (!path) return;

    const name = path.replace(/\\/g, "/").split("/").pop() ?? path;
    setFileName(name);
    setFilePath(path);
    setStatus("");
    setIsError(false);

    try {
      const content = await readText(path);
      setFileContent(content);

      const result = shiftSubtitles(content, {
        offsetMs: effectiveOffsetMs,
        thresholdMs: thresholdMs ?? undefined,
      });
      setPreview(result.preview);
      setCaptionCount(result.captionCount);
      setDetectedFormat(result.format.toUpperCase());
    } catch (e) {
      setIsError(true);
      setStatus(t("error_prefix", e instanceof Error ? e.message : String(e)));
    }
  }, [effectiveOffsetMs, thresholdMs, useThreshold, t]);

  const handleSave = useCallback(async () => {
    if (!fileContent || !filePath) return;
    if (useThreshold && thresholdMs == null) return;

    try {
      const result: ShiftResult = shiftSubtitles(fileContent, {
        offsetMs: effectiveOffsetMs,
        thresholdMs: thresholdMs ?? undefined,
      });

      // Suggest output filename
      const ext = fileName.slice(fileName.lastIndexOf("."));
      const baseName = fileName.slice(0, fileName.lastIndexOf("."));
      const defaultName = `${baseName}.shifted${ext}`;

      const savePath = await pickSavePath(defaultName);
      if (!savePath) return;

      await writeText(savePath, result.content);
      const outName = savePath.replace(/\\/g, "/").split("/").pop() ?? savePath;
      setIsError(false);
      setStatus(t("msg_saved", outName, result.captionCount));
    } catch (e) {
      setIsError(true);
      setStatus(t("error_prefix", e instanceof Error ? e.message : String(e)));
    }
  }, [fileContent, filePath, fileName, effectiveOffsetMs, thresholdMs, useThreshold, t]);

  return (
    <div className="max-w-2xl space-y-5">
      {/* File Selection */}
      <div className="space-y-2">
        <button
          onClick={handlePickFile}
          className="px-5 py-2.5 rounded-lg text-sm font-medium transition-colors"
          style={{
            background: "var(--bg-input)",
            border: "1px solid var(--border)",
            color: "var(--text-primary)",
          }}
        >
          {t("btn_select_subtitle")}
        </button>
        {fileName && (
          <div className="flex items-center gap-3 text-sm">
            <span style={{ color: "var(--text-primary)" }}>{fileName}</span>
            {detectedFormat && (
              <span
                className="px-2 py-0.5 rounded text-xs"
                style={{
                  background: "var(--bg-input)",
                  color: "var(--text-muted)",
                }}
              >
                {detectedFormat}
              </span>
            )}
            {captionCount > 0 && (
              <span className="text-xs" style={{ color: "var(--text-muted)" }}>
                {t("captions_count", captionCount)}
              </span>
            )}
          </div>
        )}
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
            onChange={(e) => setOffsetValue(parseInt(e.target.value) || 0)}
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

      {/* Save Button */}
      {fileContent && (
        <button
          onClick={handleSave}
          disabled={useThreshold && thresholdMs === null}
          className="px-6 py-2.5 rounded-lg text-white font-medium text-sm transition-colors"
          style={{ background: useThreshold && thresholdMs === null ? "var(--bg-input)" : "var(--accent)" }}
        >
          {t("btn_save_as")}
        </button>
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

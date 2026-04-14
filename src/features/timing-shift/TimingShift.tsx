import { useState, useCallback, useEffect } from "react";
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

type Unit = "ms" | "s";
type Direction = "slower" | "faster";

export default function TimingShift() {
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


  // Compute effective offset in ms
  const effectiveOffsetMs = (() => {
    const base = unit === "s" ? offsetValue * 1000 : offsetValue;
    return direction === "faster" ? -base : base;
  })();

  // Compute threshold in ms
  const thresholdMs = useThreshold ? parseTimestamp(thresholdText) : undefined;

  // Update preview whenever parameters change
  useEffect(() => {
    if (!fileContent) return;
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
  }, [fileContent, effectiveOffsetMs, thresholdMs]);

  const handlePickFile = useCallback(async () => {
    const path = await pickSubtitleFile();
    if (!path) return;

    const name = path.replace(/\\/g, "/").split("/").pop() ?? path;
    setFileName(name);
    setFilePath(path);
    setStatus("");

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
      setStatus(`Error: ${e instanceof Error ? e.message : e}`);
    }
  }, [effectiveOffsetMs, thresholdMs]);

  const handleSave = useCallback(async () => {
    if (!fileContent || !filePath) return;

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
      setStatus(`Saved: ${outName} (${result.captionCount} captions)`);
    } catch (e) {
      setStatus(`Error: ${e instanceof Error ? e.message : e}`);
    }
  }, [fileContent, filePath, fileName, effectiveOffsetMs, thresholdMs]);

  return (
    <div className="max-w-2xl space-y-5">
      {/* File Selection */}
      <div className="space-y-2">
        <button
          onClick={handlePickFile}
          className="px-5 py-2.5 rounded-lg bg-neutral-800 hover:bg-neutral-700 border border-neutral-700 text-sm font-medium text-neutral-200 transition-colors"
        >
          Select Subtitle File
        </button>
        {fileName && (
          <div className="flex items-center gap-3 text-sm">
            <span className="text-neutral-300">{fileName}</span>
            {detectedFormat && (
              <span className="px-2 py-0.5 rounded bg-neutral-800 text-xs text-neutral-400">
                {detectedFormat}
              </span>
            )}
            {captionCount > 0 && (
              <span className="text-xs text-neutral-500">
                {captionCount} captions
              </span>
            )}
          </div>
        )}
      </div>

      {/* Offset Controls */}
      <div className="flex items-end gap-3">
        <div>
          <label className="block text-sm font-medium text-neutral-300 mb-1">
            Offset
          </label>
          <input
            type="number"
            value={offsetValue}
            onChange={(e) => setOffsetValue(parseInt(e.target.value) || 0)}
            className="w-28 px-3 py-2 rounded-lg bg-neutral-800 border border-neutral-700 text-neutral-100 text-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
          />
        </div>
        <select
          value={unit}
          onChange={(e) => setUnit(e.target.value as Unit)}
          className="px-3 py-2 rounded-lg bg-neutral-800 border border-neutral-700 text-neutral-100 text-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
        >
          <option value="ms">ms</option>
          <option value="s">seconds</option>
        </select>
        <select
          value={direction}
          onChange={(e) => setDirection(e.target.value as Direction)}
          className="px-3 py-2 rounded-lg bg-neutral-800 border border-neutral-700 text-neutral-100 text-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
        >
          <option value="slower">Slower (+)</option>
          <option value="faster">Faster (−)</option>
        </select>
      </div>

      {/* Threshold */}
      <div className="flex items-center gap-3">
        <label className="flex items-center gap-2 text-sm text-neutral-300 cursor-pointer">
          <input
            type="checkbox"
            checked={useThreshold}
            onChange={(e) => setUseThreshold(e.target.checked)}
            className="rounded bg-neutral-800 border-neutral-600"
          />
          Apply only after:
        </label>
        {useThreshold && (
          <input
            type="text"
            value={thresholdText}
            onChange={(e) => setThresholdText(e.target.value)}
            placeholder="00:05:00.000"
            className="w-40 px-3 py-1.5 rounded-lg bg-neutral-800 border border-neutral-700 text-neutral-100 text-sm font-mono focus:outline-none focus:ring-2 focus:ring-blue-500"
          />
        )}
        {useThreshold && thresholdMs === null && (
          <span className="text-xs text-red-400">Invalid format (HH:MM:SS.mmm)</span>
        )}
      </div>

      {/* Preview */}
      {preview.length > 0 && (
        <div className="border border-neutral-800 rounded-lg bg-neutral-900/50">
          <div className="px-3 py-2 border-b border-neutral-800">
            <span className="text-xs font-medium text-neutral-400">
              Preview (first {preview.length} of {captionCount})
            </span>
          </div>
          <div className="max-h-64 overflow-y-auto">
            <table className="w-full text-xs font-mono">
              <thead>
                <tr className="text-neutral-500 border-b border-neutral-800">
                  <th className="px-3 py-1.5 text-left">#</th>
                  <th className="px-3 py-1.5 text-left">Original</th>
                  <th className="px-3 py-1.5 text-center">→</th>
                  <th className="px-3 py-1.5 text-left">Shifted</th>
                </tr>
              </thead>
              <tbody>
                {preview.map((entry) => (
                  <tr
                    key={entry.index}
                    className={`border-b border-neutral-800/50 ${
                      entry.wasShifted ? "" : "opacity-50"
                    }`}
                  >
                    <td className="px-3 py-1 text-neutral-500">
                      {entry.index}
                    </td>
                    <td className="px-3 py-1 text-neutral-400">
                      {formatTimestamp(entry.originalStart)}
                    </td>
                    <td className="px-3 py-1 text-center text-neutral-600">
                      {entry.wasShifted ? "→" : "·"}
                    </td>
                    <td
                      className={`px-3 py-1 ${
                        entry.wasShifted
                          ? "text-blue-400"
                          : "text-neutral-400"
                      }`}
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
          className="px-6 py-2.5 rounded-lg bg-blue-600 hover:bg-blue-500 text-white font-medium text-sm transition-colors"
        >
          Save As...
        </button>
      )}

      {/* Status */}
      {status && (
        <p
          className={`text-sm ${
            status.startsWith("Error") ? "text-red-400" : "text-green-400"
          }`}
        >
          {status}
        </p>
      )}
    </div>
  );
}

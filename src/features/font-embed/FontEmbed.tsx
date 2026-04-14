import { useState, useCallback, useRef } from "react";
import {
  pickAssFile,
  pickSavePath,
  readText,
  writeText,
} from "../../lib/tauri-api";
import {
  analyzeFonts,
  embedFonts,
  type FontInfo,
  type EmbedProgress,
} from "./font-embedder";
import {
  collectFonts,
  ensureLoaded,
  type FontUsage,
} from "./font-collector";

export default function FontEmbed() {
  const [filePath, setFilePath] = useState<string | null>(null);
  const [fileName, setFileName] = useState("");
  const [fileContent, setFileContent] = useState("");

  const [fonts, setFonts] = useState<FontInfo[]>([]);
  const [fontUsages, setFontUsages] = useState<FontUsage[]>([]);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [analyzing, setAnalyzing] = useState(false);
  const [embedding, setEmbedding] = useState(false);
  const [progress, setProgress] = useState<EmbedProgress | null>(null);
  const [status, setStatus] = useState("");
  const cancelRef = useRef(false);

  const handlePickFile = useCallback(async () => {
    await ensureLoaded();
    const path = await pickAssFile();
    if (!path) return;

    const name = path.replace(/\\/g, "/").split("/").pop() ?? path;
    setFileName(name);
    setFilePath(path);
    setFonts([]);
    setSelected(new Set());
    setStatus("");

    setAnalyzing(true);
    try {
      const content = await readText(path);
      setFileContent(content);

      // Collect font usages
      const usages = collectFonts(content);
      setFontUsages(usages);

      // Resolve system font paths
      const infos = await analyzeFonts(content);
      setFonts(infos);

      // Auto-select all found fonts
      const autoSelected = new Set<number>();
      infos.forEach((info, idx) => {
        if (info.filePath) autoSelected.add(idx);
      });
      setSelected(autoSelected);
    } catch (e) {
      setStatus(`Error: ${e instanceof Error ? e.message : e}`);
    } finally {
      setAnalyzing(false);
    }
  }, []);

  const toggleSelect = (idx: number) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(idx)) {
        next.delete(idx);
      } else {
        next.add(idx);
      }
      return next;
    });
  };

  const handleEmbed = useCallback(async () => {
    if (!fileContent || !filePath) return;

    const selectedFonts = fonts.filter(
      (_, idx) => selected.has(idx) && fonts[idx].filePath
    );
    if (selectedFonts.length === 0) {
      setStatus("No fonts selected for embedding");
      return;
    }

    setEmbedding(true);
    cancelRef.current = false;

    try {
      const result = await embedFonts(
        fileContent,
        selectedFonts,
        fontUsages,
        (p) => setProgress(p)
      );

      // Suggest output filename
      const baseName = fileName.slice(0, fileName.lastIndexOf("."));
      const defaultName = `${baseName}.embedded.ass`;

      const savePath = await pickSavePath(defaultName, [
        { name: "ASS Subtitles", extensions: ["ass"] },
      ]);
      if (!savePath) {
        setEmbedding(false);
        setProgress(null);
        return;
      }

      await writeText(savePath, result);
      const outName = savePath.replace(/\\/g, "/").split("/").pop() ?? savePath;
      setStatus(`Saved: ${outName} (${selectedFonts.length} font(s) embedded)`);
    } catch (e) {
      setStatus(`Error: ${e instanceof Error ? e.message : e}`);
    } finally {
      setEmbedding(false);
      setProgress(null);
    }
  }, [fileContent, filePath, fileName, fonts, selected, fontUsages]);

  const formatFontLabel = (info: FontInfo) => {
    let label = info.key.family;
    if (info.key.bold) label += " Bold";
    if (info.key.italic) label += " Italic";
    return label;
  };

  return (
    <div className="max-w-2xl space-y-5">
      {/* File Selection */}
      <div className="space-y-2">
        <button
          onClick={handlePickFile}
          disabled={analyzing || embedding}
          className="px-5 py-2.5 rounded-lg bg-neutral-800 hover:bg-neutral-700 disabled:opacity-50 border border-neutral-700 text-sm font-medium text-neutral-200 transition-colors"
        >
          {analyzing ? "Analyzing..." : "Select .ass File"}
        </button>
        {fileName && (
          <p className="text-sm text-neutral-300">{fileName}</p>
        )}
      </div>

      {/* Font List — always visible, shows empty state before file selection */}
      <div className="border border-neutral-800 rounded-lg bg-neutral-900/50">
        <div className="px-3 py-2 border-b border-neutral-800">
          <span className="text-xs font-medium text-neutral-400">
            Detected Fonts{fonts.length > 0 ? ` (${fonts.length})` : ""}
          </span>
        </div>
        {fonts.length > 0 ? (
          <div className="divide-y divide-neutral-800/50 max-h-64 overflow-y-auto">
            {fonts.map((info, idx) => (
              <label
                key={idx}
                className={`flex items-center gap-3 px-4 py-2.5 cursor-pointer hover:bg-neutral-800/30 transition-colors ${
                  !info.filePath ? "opacity-50" : ""
                }`}
              >
                <input
                  type="checkbox"
                  checked={selected.has(idx)}
                  onChange={() => toggleSelect(idx)}
                  disabled={!info.filePath || embedding}
                  className="rounded bg-neutral-800 border-neutral-600"
                />
                <div className="flex-1 min-w-0">
                  <span className="text-sm text-neutral-200">
                    {formatFontLabel(info)}
                  </span>
                  <span className="text-xs text-neutral-500 ml-2">
                    — {info.glyphCount} glyphs
                  </span>
                </div>
                <span
                  className={`text-xs px-2 py-0.5 rounded ${
                    info.filePath
                      ? "bg-green-900/30 text-green-400"
                      : "bg-red-900/30 text-red-400"
                  }`}
                >
                  {info.filePath ? "Found" : "Missing"}
                </span>
              </label>
            ))}
          </div>
        ) : (
          <div className="px-4 py-8 text-center">
            {analyzing ? (
              <p className="text-sm text-neutral-400">Scanning fonts...</p>
            ) : (
              <div className="space-y-1">
                <p className="text-sm text-neutral-500">No file loaded</p>
                <p className="text-xs text-neutral-600">
                  Select an .ass file to detect fonts used in the subtitle
                </p>
              </div>
            )}
          </div>
        )}
      </div>

      {/* Embed Button */}
      <button
        onClick={handleEmbed}
        disabled={embedding || selected.size === 0 || !filePath}
        className="px-6 py-2.5 rounded-lg bg-blue-600 hover:bg-blue-500 disabled:bg-neutral-700 disabled:text-neutral-500 text-white font-medium text-sm transition-colors"
      >
        {embedding
          ? "Embedding..."
          : selected.size > 0
            ? `Embed Selected Fonts (${selected.size})`
            : "Embed Fonts"}
      </button>

      {/* Progress */}
      {progress && (
        <div className="text-sm text-neutral-400">
          <p>
            {progress.stage} ({progress.current}/{progress.total})
          </p>
          <div className="mt-1 h-1.5 bg-neutral-800 rounded-full overflow-hidden">
            <div
              className="h-full bg-blue-500 transition-all"
              style={{ width: `${(progress.current / progress.total) * 100}%` }}
            />
          </div>
        </div>
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

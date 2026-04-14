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
import { useI18n } from "../../i18n/useI18n";

export default function FontEmbed() {
  const { t } = useI18n();

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
      setStatus(t("error_prefix", e instanceof Error ? e.message : String(e)));
    } finally {
      setAnalyzing(false);
    }
  }, [t]);

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
      setStatus(t("msg_no_fonts_selected"));
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
      setStatus(t("msg_embed_saved", outName, selectedFonts.length));
    } catch (e) {
      setStatus(t("error_prefix", e instanceof Error ? e.message : String(e)));
    } finally {
      setEmbedding(false);
      setProgress(null);
    }
  }, [fileContent, filePath, fileName, fonts, selected, fontUsages, t]);

  const formatFontLabel = (info: FontInfo) => {
    let label = info.key.family;
    if (info.key.bold) label += " Bold";
    if (info.key.italic) label += " Italic";
    return label;
  };

  const isEmbedDisabled = embedding || selected.size === 0 || !filePath;

  return (
    <div className="max-w-2xl space-y-5">
      {/* File Selection */}
      <div className="space-y-2">
        <button
          onClick={handlePickFile}
          disabled={analyzing || embedding}
          className="px-5 py-2.5 rounded-lg disabled:opacity-50 text-sm font-medium transition-colors"
          style={{
            background: "var(--bg-input)",
            border: "1px solid var(--border)",
            color: "var(--text-primary)",
          }}
        >
          {analyzing ? t("btn_analyzing") : t("btn_select_ass")}
        </button>
        {fileName && (
          <p className="text-sm" style={{ color: "var(--text-secondary)" }}>
            {fileName}
          </p>
        )}
      </div>

      {/* Font List — always visible, shows empty state before file selection */}
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
          <span className="text-xs font-medium" style={{ color: "var(--text-muted)" }}>
            {fonts.length > 0
              ? t("fonts_title_count", fonts.length)
              : t("fonts_title")}
          </span>
        </div>
        {fonts.length > 0 ? (
          <div className="max-h-64 overflow-y-auto">
            {fonts.map((info, idx) => (
              <label
                key={idx}
                className={`flex items-center gap-3 px-4 py-2.5 cursor-pointer transition-colors ${
                  !info.filePath ? "opacity-50" : ""
                }`}
                style={{ borderBottom: "1px solid color-mix(in srgb, var(--border) 50%, transparent)" }}
              >
                <input
                  type="checkbox"
                  checked={selected.has(idx)}
                  onChange={() => toggleSelect(idx)}
                  disabled={!info.filePath || embedding}
                  className="rounded"
                  style={{
                    background: "var(--bg-input)",
                    borderColor: "var(--border)",
                  }}
                />
                <div className="flex-1 min-w-0">
                  <span className="text-sm" style={{ color: "var(--text-primary)" }}>
                    {formatFontLabel(info)}
                  </span>
                  <span className="text-xs ml-2" style={{ color: "var(--text-muted)" }}>
                    {t("fonts_glyphs", info.glyphCount)}
                  </span>
                </div>
                <span
                  className="text-xs px-2 py-0.5 rounded"
                  style={
                    info.filePath
                      ? { background: "var(--badge-green-bg)", color: "var(--badge-green-text)" }
                      : { background: "var(--badge-red-bg)", color: "var(--badge-red-text)" }
                  }
                >
                  {info.filePath ? t("fonts_found") : t("fonts_missing")}
                </span>
              </label>
            ))}
          </div>
        ) : (
          <div className="px-4 py-8 text-center">
            {analyzing ? (
              <p className="text-sm" style={{ color: "var(--text-muted)" }}>
                {t("fonts_scanning")}
              </p>
            ) : (
              <div className="space-y-1">
                <p className="text-sm" style={{ color: "var(--text-muted)" }}>
                  {t("fonts_empty")}
                </p>
                <p className="text-xs" style={{ color: "var(--text-muted)", opacity: 0.7 }}>
                  {t("fonts_empty_hint")}
                </p>
              </div>
            )}
          </div>
        )}
      </div>

      {/* Embed Button */}
      <button
        onClick={handleEmbed}
        disabled={isEmbedDisabled}
        className="px-6 py-2.5 rounded-lg font-medium text-sm transition-colors"
        style={
          isEmbedDisabled
            ? { background: "var(--accent-disabled-bg)", color: "var(--accent-disabled-text)" }
            : { background: "var(--accent)", color: "#fff" }
        }
      >
        {embedding
          ? t("btn_embedding")
          : selected.size > 0
            ? t("btn_embed", selected.size)
            : t("btn_embed_default")}
      </button>

      {/* Progress */}
      {progress && (
        <div className="text-sm" style={{ color: "var(--text-muted)" }}>
          <p>
            {progress.stage} ({progress.current}/{progress.total})
          </p>
          <div
            className="mt-1 h-1.5 rounded-full overflow-hidden"
            style={{ background: "var(--progress-bg)" }}
          >
            <div
              className="h-full transition-all"
              style={{
                background: "var(--progress-fill)",
                width: `${(progress.current / progress.total) * 100}%`,
              }}
            />
          </div>
        </div>
      )}

      {/* Status */}
      {status && (
        <p
          className="text-sm"
          style={{
            color: status.startsWith("Error")
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

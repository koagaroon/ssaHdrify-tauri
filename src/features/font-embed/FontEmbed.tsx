import { useState, useCallback, useRef } from "react";
import {
  pickAssFile,
  pickSavePath,
  readText,
  writeText,
  fileNameFromPath,
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
  fontKeyLabel,
  type FontUsage,
} from "./font-collector";
import { useI18n } from "../../i18n/useI18n";
import { useFileContext } from "../../lib/FileContext";

export default function FontEmbed() {
  const { t } = useI18n();
  const { fontsFile, setFontsFile, clearFile, isFileInUse } = useFileContext();

  const [fonts, setFonts] = useState<FontInfo[]>([]);
  const [fontUsages, setFontUsages] = useState<FontUsage[]>([]);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [analyzing, setAnalyzing] = useState(false);
  const [embedding, setEmbedding] = useState(false);
  const [progress, setProgress] = useState<EmbedProgress | null>(null);
  const [status, setStatus] = useState("");
  const [isError, setIsError] = useState(false);
  const cancelRef = useRef(false);
  // Generation counter: incremented on each pick or clear to invalidate stale async results
  const pickGenRef = useRef(0);

  // Derive file state from context
  const filePath = fontsFile?.filePath ?? null;
  const fileName = fontsFile?.fileName ?? "";
  const fileContent = fontsFile?.fileContent ?? "";

  const handlePickFile = useCallback(async () => {
    // Claim generation BEFORE any await so clear-during-dialog is guarded.
    // If the user clicks × (clear) while ensureLoaded or the file dialog is
    // open, handleClearFile increments pickGenRef, and the stale pick will
    // be rejected at every subsequent guard check.
    const gen = pickGenRef.current = pickGenRef.current + 1;

    await ensureLoaded();
    if (gen !== pickGenRef.current) return; // cleared while loading module

    const path = await pickAssFile();
    if (!path) return;
    if (gen !== pickGenRef.current) return; // cleared during dialog

    // Cross-tab duplicate guard
    const usedIn = isFileInUse(path, "fonts");
    if (usedIn) {
      setIsError(true);
      setStatus(t("msg_file_in_use", t("tab_" + usedIn)));
      return;
    }

    setFonts([]);
    setSelected(new Set());
    setStatus("");
    setIsError(false);

    setAnalyzing(true);
    try {
      const content = await readText(path);
      if (gen !== pickGenRef.current) return; // stale — user cleared or re-picked

      const name = fileNameFromPath(path);

      // Collect font usages
      const usages = collectFonts(content);
      setFontUsages(usages);

      // Resolve system font paths (slow Rust IPC for each font)
      const infos = await analyzeFonts(content);
      if (gen !== pickGenRef.current) return; // stale — user cleared or re-picked

      setFonts(infos);

      // Auto-select all found fonts
      const autoSelected = new Set<number>();
      infos.forEach((info, idx) => {
        if (info.filePath) autoSelected.add(idx);
      });
      setSelected(autoSelected);

      // Silent replace: see FileContext.tsx for design rationale
      setFontsFile({
        filePath: path,
        fileName: name,
        fileContent: content,
      });
    } catch (e) {
      if (gen !== pickGenRef.current) return; // stale — don't show error for cancelled pick
      setIsError(true);
      setStatus(t("error_prefix", e instanceof Error ? e.message : String(e)));
    } finally {
      if (gen === pickGenRef.current) setAnalyzing(false);
    }
  }, [isFileInUse, setFontsFile, t]);

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
    setIsError(false);
    cancelRef.current = false;

    try {
      const result = await embedFonts(
        fileContent,
        selectedFonts,
        fontUsages,
        (p) => setProgress(p),
        () => cancelRef.current,
        t
      );

      // If cancelled, clean up and exit without showing save dialog
      if (result === null) {
        setStatus("");
        setIsError(false);
        return;
      }

      // Suggest output filename
      const baseName = fileName.slice(0, fileName.lastIndexOf("."));
      const defaultName = `${baseName}.embedded.ass`;

      const savePath = await pickSavePath(defaultName, [
        { name: "ASS Subtitles", extensions: ["ass"] },
      ]);
      if (!savePath) {
        return;
      }

      await writeText(savePath, result.content);
      const outName = fileNameFromPath(savePath);
      setIsError(false);
      setStatus(t("msg_embed_saved", outName, result.embeddedCount));
    } catch (e) {
      setIsError(true);
      setStatus(t("error_prefix", e instanceof Error ? e.message : String(e)));
    } finally {
      setEmbedding(false);
      setProgress(null);
    }
  }, [fileContent, filePath, fileName, fonts, selected, fontUsages, t]);

  const formatFontLabel = (info: FontInfo) => fontKeyLabel(info.key);

  const handleClearFile = useCallback(() => {
    // Increment generation to invalidate any in-flight handlePickFile async work
    pickGenRef.current = pickGenRef.current + 1;
    clearFile("fonts");
    setFonts([]);
    setFontUsages([]);
    setSelected(new Set());
    setAnalyzing(false);
    setStatus("");
    setIsError(false);
    setProgress(null);
  }, [clearFile]);

  const isEmbedDisabled = embedding || selected.size === 0 || !filePath;

  function embedButtonLabel(): string {
    if (embedding) return t("btn_embedding");
    if (selected.size > 0) return t("btn_embed", selected.size);
    return t("btn_embed_default");
  }

  return (
    <div className="space-y-5">
      {/* ── Top area: file info left + buttons right ── */}
      <div className="flex items-start justify-between gap-6">
        {/* Left: file name */}
        <div className="flex items-center gap-3 flex-1 min-w-0">
          {fileName && (
            <>
              <span
                className="text-sm truncate"
                style={{ color: "var(--text-primary)" }}
              >
                {fileName}
              </span>
              <button
                onClick={handleClearFile}
                disabled={embedding}
                className="flex-none px-3 py-2 rounded-lg text-lg font-bold transition-colors"
                style={{
                  background: "var(--cancel-bg)",
                  color: "var(--cancel-text)",
                  opacity: embedding ? 0.4 : 1,
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
            disabled={analyzing || embedding}
            className="w-full px-5 py-2.5 rounded-lg font-medium text-sm transition-colors"
            style={{
              background: analyzing || embedding ? "var(--bg-input)" : "var(--accent)",
              color: analyzing || embedding ? "var(--text-muted)" : "white",
            }}
          >
            {analyzing ? t("btn_analyzing") : t("btn_select_file")}
          </button>
          <button
            onClick={handleEmbed}
            disabled={isEmbedDisabled}
            className="w-full px-5 py-2.5 rounded-lg font-medium text-sm transition-colors"
            style={
              isEmbedDisabled
                ? { background: "var(--accent-disabled-bg)", color: "var(--accent-disabled-text)", opacity: !filePath ? 0.5 : 1 }
                : { background: "var(--accent)", color: "#fff" }
            }
          >
            {embedButtonLabel()}
          </button>
          {embedding && (
            <button
              onClick={() => { cancelRef.current = true; }}
              className="w-full px-5 py-2.5 rounded-lg font-medium text-sm transition-colors"
              style={{
                background: "var(--cancel-bg)",
                color: "var(--cancel-text)",
              }}
            >
              {t("btn_cancel")}
            </button>
          )}
        </div>
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

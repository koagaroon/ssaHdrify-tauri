import { useState, useCallback, useRef, useMemo } from "react";
import {
  pickAssFile,
  pickSavePath,
  readText,
  writeText,
  fileNameFromPath,
} from "../../lib/tauri-api";
import {
  analyzeFonts,
  buildUserFontMap,
  embedFonts,
  type FontInfo,
  type EmbedProgress,
} from "./font-embedder";
import { ensureLoaded, fontKeyLabel, type FontUsage } from "./font-collector";
import { useI18n } from "../../i18n/useI18n";
import { useFileContext } from "../../lib/FileContext";
import FontSourceModal, { type FontSource } from "./FontSourceModal";

/** Indices of fonts that resolved to a file — used to pre-check them in the UI. */
function indicesOfResolvedFonts(infos: FontInfo[]): Set<number> {
  const out = new Set<number>();
  infos.forEach((info, idx) => {
    if (info.filePath) out.add(idx);
  });
  return out;
}

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

  // ── Local font sources (persist for the tab session) ─────
  const [fontSources, setFontSources] = useState<FontSource[]>([]);
  const [sourceModalOpen, setSourceModalOpen] = useState(false);

  // Derived: flattened user font map. Built once per sources change via the
  // canonical helper so every match site (initial analyze, reanalyze, etc.)
  // uses identical indexing logic. Each face contributes multiple keys —
  // one per localized family name variant — all pointing at the same entry.
  const userFontMap = useMemo(
    () => buildUserFontMap(fontSources.flatMap((src) => src.entries)),
    [fontSources]
  );

  // Derive file state from context
  const filePath = fontsFile?.filePath ?? null;
  const fileName = fontsFile?.fileName ?? "";
  const fileContent = fontsFile?.fileContent ?? "";

  const handlePickFile = useCallback(async () => {
    // Claim generation BEFORE any await so clear-during-dialog is guarded.
    // If the user clicks × (clear) while ensureLoaded or the file dialog is
    // open, handleClearFile increments pickGenRef, and the stale pick will
    // be rejected at every subsequent guard check.
    const gen = (pickGenRef.current = pickGenRef.current + 1);

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

      // Resolve fonts — local user sources take priority, system fonts fall
      // back after. See font-embedder.ts for the match order.
      const { infos, usages } = await analyzeFonts(content, userFontMap);
      if (gen !== pickGenRef.current) return; // stale — user cleared or re-picked

      setFontUsages(usages);
      setFonts(infos);
      setSelected(indicesOfResolvedFonts(infos));

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
  }, [isFileInUse, setFontsFile, t, userFontMap]);

  // ── Font source management ────────────────────────────
  // Adding a source: append to the list, then — if an ASS is already loaded
  // — rerun analyzeFonts with the fresh user font map so the main list
  // updates its found/missing badges without requiring the user to re-pick
  // the subtitle file.
  const reanalyzeWithSources = useCallback(
    async (nextSources: FontSource[]) => {
      if (!fileContent) return;
      const map = buildUserFontMap(nextSources.flatMap((src) => src.entries));
      try {
        const { infos, usages } = await analyzeFonts(fileContent, map);
        setFontUsages(usages);
        setFonts(infos);
        setSelected(indicesOfResolvedFonts(infos));
      } catch (e) {
        setIsError(true);
        setStatus(t("error_prefix", e instanceof Error ? e.message : String(e)));
      }
    },
    [fileContent, t]
  );

  const handleAddFontSource = useCallback(
    (source: FontSource): { added: number; duplicated: number } => {
      // Dedup against faces already registered in any existing source.
      // A face is uniquely identified by (path, index) — multiple family-name
      // variants live INSIDE one face, so we must not dedup on family.
      const registered = new Set<string>();
      for (const src of fontSources) {
        for (const e of src.entries) {
          registered.add(`${e.path}|${e.index}`);
        }
      }
      const newEntries = source.entries.filter((e) => !registered.has(`${e.path}|${e.index}`));
      const duplicated = source.entries.length - newEntries.length;
      if (newEntries.length === 0) {
        return { added: 0, duplicated };
      }

      // Build nextSources from the closure value — side-effect-free state
      // update, then fire the async reanalyze outside the setter. StrictMode
      // double-invokes setState updaters, so putting the reanalyze inside
      // would run it twice on every add.
      const filtered: FontSource = { ...source, entries: newEntries };
      const nextSources = [...fontSources, filtered];
      setFontSources(nextSources);
      void reanalyzeWithSources(nextSources);
      return { added: newEntries.length, duplicated };
    },
    [fontSources, reanalyzeWithSources]
  );

  const handleRemoveFontSource = useCallback(
    (id: string) => {
      const nextSources = fontSources.filter((s) => s.id !== id);
      setFontSources(nextSources);
      void reanalyzeWithSources(nextSources);
    },
    [fontSources, reanalyzeWithSources]
  );

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

    const selectedFonts = fonts.filter((info, idx) => selected.has(idx) && info.filePath);
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
    <div className="space-y-4">
      {/* ── File strip — always visible; filename + clear + Select Subtitle ── */}
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
            <span className="truncate flex-1" style={{ color: "var(--text-primary)" }}>
              {fileName}
            </span>
          ) : (
            <span className="italic" style={{ color: "var(--text-muted)" }}>
              {t("file_empty")}
            </span>
          )}
        </div>
        {fileName && (
          <button
            onClick={handleClearFile}
            disabled={embedding}
            className="flex-none px-3 rounded-lg text-lg font-bold transition-colors"
            style={{
              background: "var(--cancel-bg)",
              color: "var(--cancel-text)",
              opacity: embedding ? 0.4 : 1,
              height: "38px",
            }}
            title={t("btn_clear_file")}
          >
            ✕
          </button>
        )}
        <button
          onClick={handlePickFile}
          disabled={analyzing || embedding}
          className="flex-none px-5 rounded-lg font-medium text-sm transition-colors"
          style={{
            background: analyzing || embedding ? "var(--bg-input)" : "var(--accent)",
            color: analyzing || embedding ? "var(--text-muted)" : "white",
            height: "38px",
          }}
        >
          {analyzing ? t("btn_analyzing") : t("btn_select_subtitle_file")}
        </button>
      </div>

      {/* ── Action row: Select Font Files + Embed (+ Cancel during embed) ── */}
      <div className="flex items-center gap-2">
        <button
          onClick={() => setSourceModalOpen(true)}
          disabled={embedding || !filePath}
          className="px-5 rounded-lg font-medium text-sm transition-colors"
          style={
            embedding || !filePath
              ? {
                  background: "var(--accent-disabled-bg)",
                  color: "var(--accent-disabled-text)",
                  opacity: 0.5,
                  height: "38px",
                }
              : { background: "var(--accent)", color: "#fff", height: "38px" }
          }
          title={!filePath ? t("font_coverage_no_subtitle") : undefined}
        >
          {fontSources.length > 0
            ? t("btn_select_font_files_with_count", fontSources.length)
            : t("btn_select_font_files")}
        </button>
        <div className="flex-1" />
        {embedding && (
          <button
            onClick={() => {
              cancelRef.current = true;
            }}
            className="px-4 rounded-lg text-sm transition-colors"
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
          onClick={handleEmbed}
          disabled={isEmbedDisabled}
          className="px-6 rounded-lg font-medium text-sm transition-colors"
          style={
            isEmbedDisabled
              ? {
                  background: "var(--accent-disabled-bg)",
                  color: "var(--accent-disabled-text)",
                  opacity: !filePath ? 0.5 : 1,
                  height: "38px",
                  minWidth: "140px",
                }
              : { background: "var(--accent)", color: "#fff", height: "38px", minWidth: "140px" }
          }
        >
          {embedButtonLabel()}
        </button>
      </div>
      {fonts.length > 0 && (
        <p className="text-xs -mt-2" style={{ color: "var(--text-secondary)" }}>
          {t("fonts_full_embed_warning")}
        </p>
      )}

      {/* Font List — always visible, shows empty state before file selection */}
      <div
        className="rounded-lg"
        style={{
          border: "1px solid var(--border)",
          background: "var(--bg-panel)",
        }}
      >
        <div className="px-3 py-2" style={{ borderBottom: "1px solid var(--border)" }}>
          <span className="text-xs font-medium" style={{ color: "var(--text-muted)" }}>
            {fonts.length > 0 ? t("fonts_title_count", fonts.length) : t("fonts_title")}
          </span>
        </div>
        {fonts.length > 0 ? (
          <>
            <div className="font-row font-row-header" aria-hidden="true">
              <span />
              <span>{t("col_font_name")}</span>
              <span>{t("col_font_glyphs")}</span>
              <span>{t("col_font_source")}</span>
              <span>{t("col_font_status")}</span>
            </div>
            <div className="max-h-64 overflow-y-auto">
              {fonts.map((info, idx) => (
                <label
                  key={idx}
                  className={"font-row" + (!info.filePath ? " missing" : "")}
                >
                <input
                  type="checkbox"
                  id={`font-row-${idx}`}
                  name={`font-${idx}`}
                  checked={selected.has(idx)}
                  onChange={() => toggleSelect(idx)}
                  disabled={!info.filePath || embedding}
                  className="rounded"
                  style={{
                    background: "var(--bg-input)",
                    borderColor: "var(--border)",
                  }}
                />
                <span className="font-name" title={formatFontLabel(info)}>
                  {formatFontLabel(info)}
                </span>
                <span className="font-stat">{t("fonts_glyphs", info.glyphCount)}</span>
                {info.source ? (
                  <span className="badge badge-mute">
                    {t(info.source === "local" ? "badge_local" : "badge_system")}
                  </span>
                ) : (
                  <span />
                )}
                <span className={"badge " + (info.filePath ? "badge-green" : "badge-red")}>
                  {info.filePath ? t("fonts_found") : t("fonts_missing")}
                </span>
              </label>
            ))}
            </div>
          </>
        ) : (
          <div className="px-4 py-8 text-center">
            {analyzing ? (
              <p className="text-sm" style={{ color: "var(--text-secondary)" }}>
                {t("fonts_scanning")}
              </p>
            ) : (
              <div className="space-y-1">
                <p className="text-sm" style={{ color: "var(--text-primary)" }}>
                  {t("fonts_empty")}
                </p>
                <p className="text-xs" style={{ color: "var(--text-secondary)" }}>
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
            color: isError ? "var(--error)" : "var(--success)",
          }}
        >
          {status}
        </p>
      )}

      {/* Font source modal */}
      <FontSourceModal
        open={sourceModalOpen}
        onClose={() => setSourceModalOpen(false)}
        sources={fontSources}
        usages={fontUsages}
        userFontMap={userFontMap}
        hasSubtitle={!!filePath}
        onAddSource={handleAddFontSource}
        onRemoveSource={handleRemoveFontSource}
      />
    </div>
  );
}

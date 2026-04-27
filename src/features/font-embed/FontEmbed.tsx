import { useState, useCallback, useEffect, useRef, useMemo } from "react";
import { pickAssFiles, readText, writeText, fileNameFromPath } from "../../lib/tauri-api";
import { ask } from "@tauri-apps/plugin-dialog";
import {
  analyzeFonts,
  buildUserFontMap,
  embedFonts,
  userFontKey,
  deriveEmbeddedPath,
  type FontInfo,
  type EmbedProgress,
} from "./font-embedder";
import { ensureLoaded, fontKeyLabel, type FontUsage } from "./font-collector";
import { useI18n } from "../../i18n/useI18n";
import { useFileContext } from "../../lib/FileContext";
import { TAB_LABEL_KEYS } from "../../lib/tab-labels";
import type { TabId } from "../../lib/FileContext";
import type { Status } from "../../lib/StatusContext";
import { useTabStatus } from "../../lib/useTabStatus";
import FontSourceModal, { type FontSource } from "./FontSourceModal";
import { useFolderDrop } from "../../lib/useFolderDrop";
import { countExistingFiles } from "../../lib/output-collisions";

/** Stable selection key — survives `fonts[]` reorders (e.g. after adding a
 *  new font source triggers a reanalyze with a different ordering). Using
 *  array indices here was the prior bug: indices shifted on reorder and the
 *  user embedded fonts they hadn't checked. */
function fontSelectionKey(info: FontInfo): string {
  return userFontKey(info.key.family, info.key.bold, info.key.italic);
}

/** Stable keys of fonts that resolved to a file — used to pre-check them in the UI. */
function keysOfResolvedFonts(infos: FontInfo[]): Set<string> {
  const out = new Set<string>();
  for (const info of infos) {
    if (info.filePath) out.add(fontSelectionKey(info));
  }
  return out;
}

interface LogEntry {
  id: number;
  text: string;
  type: "info" | "error" | "success";
}

// Font Embed only operates on ASS / SSA — other subtitle formats don't carry
// font references. Used by the folder-drop filter so a show folder dropped
// here keeps videos and SRTs out of the batch.
const ASS_EXTS = new Set(["ass", "ssa"]);
function fileNameHasAssExt(name: string): boolean {
  const dot = name.lastIndexOf(".");
  if (dot < 0) return false;
  return ASS_EXTS.has(name.slice(dot + 1).toLowerCase());
}

export default function FontEmbed() {
  const { t } = useI18n();
  const { fontsFiles, setFontsFiles, clearFile, isFileInUse } = useFileContext();

  // Per-file state — populated for the FIRST file when a selection lands.
  // In single-file mode the user interacts with this grid + checkbox set
  // directly. In batch mode the grid is hidden; remaining files are
  // analyzed during the embed loop using userFontMap built from sources.
  const [fonts, setFonts] = useState<FontInfo[]>([]);
  const [fontUsages, setFontUsages] = useState<FontUsage[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [analyzing, setAnalyzing] = useState(false);
  const [embedding, setEmbedding] = useState(false);
  // Per-font subsetting progress — only surfaced in single-file mode.
  // Batch suppresses this to avoid a noisy progress jitter as it cycles
  // per file; the footer N-of-M chip is the file-level signal there.
  const [progress, setProgress] = useState<EmbedProgress | null>(null);
  // File-level N-of-M progress for batch.
  const [batchProgress, setBatchProgress] = useState<{ processed: number; total: number } | null>(
    null
  );
  const [lastActionResult, setLastActionResult] = useState<
    "success" | "error" | "cancelled" | null
  >(null);
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [showFileList, setShowFileList] = useState(false);
  const [dropActive, setDropActive] = useState(false);
  const [dropError, setDropError] = useState<string | null>(null);

  const cancelRef = useRef(false);
  const pickGenRef = useRef(0);
  const logIdRef = useRef(0);
  const dropZoneRef = useRef<HTMLDivElement>(null);
  // Scroll container for the log — see HdrConvert for the rationale
  // behind avoiding scrollIntoView (it walks ancestors and can scroll
  // .window past the titlebar in Chromium).
  const logScrollRef = useRef<HTMLDivElement>(null);
  const fileContainerRef = useRef<HTMLDivElement>(null);

  // ── Local font sources (persist for the tab session) ─────
  const [fontSources, setFontSources] = useState<FontSource[]>([]);
  const [sourceModalOpen, setSourceModalOpen] = useState(false);

  // Derived: flattened user font map. Built once per sources change via the
  // canonical helper so every match site (initial analyze, reanalyze, batch
  // loop) uses identical indexing logic. Each face contributes multiple keys
  // — one per localized family name variant — all pointing at the same entry.
  const userFontMap = useMemo(
    () => buildUserFontMap(fontSources.flatMap((src) => src.entries)),
    [fontSources]
  );

  const filePaths = useMemo(() => fontsFiles?.filePaths ?? [], [fontsFiles]);
  const fileNames = useMemo(() => fontsFiles?.fileNames ?? [], [fontsFiles]);
  const firstFileContent = fontsFiles?.firstFileContent ?? "";
  const primaryFileName = fileNames[0] ?? "";
  const fileCount = filePaths.length;
  const isSingleFile = fileCount === 1;
  const isBatch = fileCount > 1;

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

  // Strict cross-tab dedup. If any path is loaded in another tab, the
  // whole selection is rejected — same UX contract as HDR Convert and
  // Time Shift: visible banner naming the conflicting tab, no state
  // change, the previous selection is preserved.
  const checkConflicts = useCallback(
    (paths: string[]): string | null => {
      let conflictCount = 0;
      let conflictTab: TabId | null = null;
      for (const p of paths) {
        const usedIn = isFileInUse(p, "fonts");
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

  // Shared ingestion path. Analyzes ONLY the first file for the detection
  // grid (single-file mode); batch entries are re-analyzed during the
  // embed loop so memory stays flat and font-source changes between pick
  // and embed are honored per file.
  const ingestPaths = useCallback(
    async (paths: string[], gen: number) => {
      const conflictMsg = checkConflicts(paths);
      if (conflictMsg) {
        setDropError(conflictMsg);
        return;
      }
      setDropError(null);

      setFonts([]);
      setFontUsages([]);
      setSelected(new Set());

      setAnalyzing(true);
      try {
        await ensureLoaded();
        if (gen !== pickGenRef.current) return;

        let firstContent: string;
        try {
          firstContent = await readText(paths[0]);
        } catch (e) {
          addLog(t("error_prefix", e instanceof Error ? e.message : String(e)), "error");
          return;
        }
        if (gen !== pickGenRef.current) return;

        const { infos, usages } = await analyzeFonts(firstContent, userFontMap);
        if (gen !== pickGenRef.current) return;

        setFontUsages(usages);
        setFonts(infos);
        setSelected(keysOfResolvedFonts(infos));

        const names = paths.map(fileNameFromPath);
        setFontsFiles({
          filePaths: paths,
          fileNames: names,
          firstFileContent: firstContent,
        });
      } catch (e) {
        if (gen !== pickGenRef.current) return;
        addLog(t("error_prefix", e instanceof Error ? e.message : String(e)), "error");
      } finally {
        if (gen === pickGenRef.current) setAnalyzing(false);
      }
    },
    [checkConflicts, setFontsFiles, addLog, t, userFontMap]
  );

  const handlePickFiles = useCallback(async () => {
    const gen = (pickGenRef.current = pickGenRef.current + 1);
    const paths = await pickAssFiles();
    if (gen !== pickGenRef.current) return;
    if (!paths || paths.length === 0) return;
    await ingestPaths(paths, gen);
  }, [ingestPaths]);

  const handleDroppedPaths = useCallback(
    async (paths: string[]) => {
      const assPaths = paths.filter((p) => fileNameHasAssExt(fileNameFromPath(p)));
      if (assPaths.length === 0) {
        addLog(t("msg_no_subtitle_in_drop"), "error");
        return;
      }
      const gen = (pickGenRef.current = pickGenRef.current + 1);
      await ingestPaths(assPaths, gen);
    },
    [ingestPaths, addLog, t]
  );

  useFolderDrop({
    ref: dropZoneRef,
    onPaths: handleDroppedPaths,
    onActiveChange: setDropActive,
    disabled: embedding,
  });

  // ── Font source management (only affects first file's grid) ──────
  // Adding/removing a source: append/remove to the list, then re-run
  // analyzeFonts on the FIRST file with the fresh user font map so the
  // grid updates its found/missing badges. Batch files pick up the new
  // sources naturally during the embed loop (which builds userFontMap
  // afresh each iteration via the global memo).
  const reanalyzeWithSources = useCallback(
    async (nextSources: FontSource[]) => {
      if (!firstFileContent) return;
      const gen = (pickGenRef.current = pickGenRef.current + 1);
      const map = buildUserFontMap(nextSources.flatMap((src) => src.entries));
      try {
        const { infos, usages } = await analyzeFonts(firstFileContent, map);
        if (gen !== pickGenRef.current) return;
        setFontUsages(usages);
        setFonts(infos);
        // Merge: keep manual unchecks; auto-check newly resolved fonts.
        // See the original implementation comment in font-embedder for
        // the full rationale.
        setSelected((prev) => {
          const resolved = keysOfResolvedFonts(infos);
          const next = new Set<string>();
          for (const key of prev) {
            if (resolved.has(key)) next.add(key);
          }
          for (const key of resolved) {
            if (!prev.has(key)) next.add(key);
          }
          return next;
        });
      } catch (e) {
        if (gen !== pickGenRef.current) return;
        addLog(t("error_prefix", e instanceof Error ? e.message : String(e)), "error");
      }
    },
    [firstFileContent, addLog, t]
  );

  const handleAddFontSource = useCallback(
    (source: FontSource): { added: number; duplicated: number } => {
      // Dedup against faces already registered in any existing source.
      // A face is uniquely identified by (path, index) — multiple
      // family-name variants live INSIDE one face, so we must not dedup
      // on family.
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

  const toggleSelect = (key: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(key)) {
        next.delete(key);
      } else {
        next.add(key);
      }
      return next;
    });
  };

  // Reset last-action on selection change so done/cancelled doesn't stick.
  useEffect(() => {
    setLastActionResult(null);
  }, [fontsFiles]);

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

  // ── Embed handler — handles both single-file and batch modes ─────
  const handleEmbed = useCallback(async () => {
    if (fileCount === 0) return;

    // Pre-flight overwrite check — same project-wide pattern.
    const projectedOutputs = filePaths.map((p) => deriveEmbeddedPath(p));
    const existingCount = await countExistingFiles(projectedOutputs);
    if (existingCount > 0) {
      const confirmed = await ask(t("msg_overwrite_confirm", existingCount, filePaths.length), {
        title: t("dialog_overwrite_title"),
        kind: "warning",
      });
      if (!confirmed) {
        addLog(t("msg_fonts_cancelled"), "info");
        setLastActionResult("cancelled");
        return;
      }
    }

    setEmbedding(true);
    setBatchProgress({ processed: 0, total: filePaths.length });
    cancelRef.current = false;

    try {
      addLog(t("msg_fonts_start", filePaths.length));

      let successCount = 0;
      let processedCount = 0;
      const seenOutputs = new Set<string>();

      for (let i = 0; i < filePaths.length; i++) {
        const filePath = filePaths[i];

        if (cancelRef.current) {
          addLog(t("msg_fonts_cancelled"), "info");
          break;
        }

        const fileName = fileNameFromPath(filePath);
        addLog(t("msg_processing", fileName));

        try {
          const outputPath = deriveEmbeddedPath(filePath);
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

          // Pick the right (infos, usages) for this file:
          // - Single-file mode (i === 0 only): use the in-memory grid
          //   state so the user's checkbox unchecks are honored.
          // - Batch mode: re-analyze each file fresh; selection is
          //   "embed all found fonts" (no per-font UI in batch).
          let fileInfos: FontInfo[];
          let fileUsages: FontUsage[];
          if (isSingleFile && i === 0) {
            fileInfos = fonts;
            fileUsages = fontUsages;
          } else {
            const analyzed = await analyzeFonts(content, userFontMap);
            fileInfos = analyzed.infos;
            fileUsages = analyzed.usages;
          }
          if (cancelRef.current) break;

          const selectedFonts = isSingleFile
            ? fileInfos.filter((info) => selected.has(fontSelectionKey(info)) && info.filePath)
            : fileInfos.filter((info) => info.filePath);

          if (selectedFonts.length === 0) {
            addLog(t("msg_no_fonts_selected"), "error");
            continue;
          }

          // Per-font subsetting progress — only in single-file. In
          // batch we suppress to avoid a noisy progress bar that resets
          // per file; the footer N-of-M chip is the file-level signal.
          const onProgress = isSingleFile ? (p: EmbedProgress) => setProgress(p) : undefined;

          const result = await embedFonts(
            content,
            selectedFonts,
            fileUsages,
            onProgress,
            () => cancelRef.current,
            t
          );

          if (result === null) {
            // Cancelled mid-embed for this file — break out of batch.
            break;
          }
          if (cancelRef.current) break;

          await writeText(outputPath, result.content);
          const outName = fileNameFromPath(outputPath);
          addLog(t("msg_embed_saved", outName, result.embeddedCount), "success");
          successCount++;
        } catch (e) {
          addLog(
            t("msg_fonts_error", fileName, e instanceof Error ? e.message : String(e)),
            "error"
          );
        } finally {
          processedCount++;
          setBatchProgress({ processed: processedCount, total: filePaths.length });
        }
      }

      if (!cancelRef.current) {
        addLog(t("msg_fonts_complete", successCount, filePaths.length), "success");
      }

      // Cancel takes precedence over success/error.
      if (cancelRef.current) {
        setLastActionResult("cancelled");
      } else {
        setLastActionResult(successCount > 0 ? "success" : "error");
      }
    } finally {
      setEmbedding(false);
      setBatchProgress(null);
      setProgress(null);
    }
  }, [fileCount, filePaths, isSingleFile, fonts, fontUsages, selected, userFontMap, addLog, t]);

  // Footer status — busy carries N-of-M progress; cancelled is its own
  // visible state.
  const tabStatus = useMemo<Status>(() => {
    if (fileCount === 0) return { kind: "idle", message: t("status_fonts_idle") };
    if (embedding) {
      return {
        kind: "busy",
        message: t("status_fonts_busy"),
        progress: batchProgress ?? undefined,
      };
    }
    if (analyzing) return { kind: "busy", message: t("status_fonts_analyzing") };
    if (lastActionResult === "success") return { kind: "done", message: t("status_fonts_done") };
    if (lastActionResult === "error") return { kind: "error", message: t("status_fonts_error") };
    if (lastActionResult === "cancelled") {
      return { kind: "pending", message: t("status_fonts_cancelled") };
    }
    if (isSingleFile && selected.size === 0) {
      return { kind: "pending", message: t("status_fonts_pick") };
    }
    if (isSingleFile) {
      return { kind: "pending", message: t("status_fonts_pending", selected.size) };
    }
    return { kind: "pending", message: t("status_fonts_batch_ready", fileCount) };
  }, [fileCount, isSingleFile, analyzing, embedding, batchProgress, selected, lastActionResult, t]);
  useTabStatus("fonts", tabStatus);

  const formatFontLabel = (info: FontInfo) => fontKeyLabel(info.key);

  const handleClearFiles = useCallback(() => {
    pickGenRef.current = pickGenRef.current + 1;
    clearFile("fonts");
    setFonts([]);
    setFontUsages([]);
    setSelected(new Set());
    setAnalyzing(false);
    setProgress(null);
    setDropError(null);
  }, [clearFile]);

  const isEmbedDisabled = embedding || (isSingleFile && selected.size === 0) || fileCount === 0;

  function embedButtonLabel(): string {
    if (embedding) return t("btn_embedding");
    if (isBatch) return t("btn_embed_all", fileCount);
    if (selected.size > 0) return t("btn_embed", selected.size);
    return t("btn_embed_default");
  }

  return (
    <div className="space-y-4">
      {/* File strip — drop zone + filename(s) + clear + Select */}
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
                <span className="truncate flex-1" style={{ color: "var(--text-primary)" }}>
                  {primaryFileName}
                </span>
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
                  {t("hdr_files_title", fileCount)}
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
          onClick={handlePickFiles}
          disabled={analyzing || embedding}
          className="flex-none px-5 rounded-lg font-medium text-sm transition-colors"
          style={{
            background: analyzing || embedding ? "var(--bg-input)" : "var(--accent)",
            color: analyzing || embedding ? "var(--text-muted)" : "white",
            height: "38px",
          }}
        >
          {analyzing ? t("btn_analyzing") : t("btn_select_files")}
        </button>
      </div>

      {/* Selection-rejected banner */}
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

      {/* Drop hint when idle */}
      {fileCount === 0 && !dropError && (
        <p className="text-xs ml-1" style={{ color: "var(--text-muted)" }}>
          {t("fonts_drop_hint")}
        </p>
      )}

      {/* Action row: select fonts + embed + cancel */}
      <div className="flex items-center gap-2">
        <button
          onClick={() => setSourceModalOpen(true)}
          disabled={embedding || fileCount === 0}
          className="px-5 rounded-lg font-medium text-sm transition-colors"
          style={
            embedding || fileCount === 0
              ? {
                  background: "var(--accent-disabled-bg)",
                  color: "var(--accent-disabled-text)",
                  opacity: 0.5,
                  height: "38px",
                }
              : { background: "var(--accent)", color: "#fff", height: "38px" }
          }
          title={fileCount === 0 ? t("font_coverage_no_subtitle") : undefined}
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
                  opacity: fileCount === 0 ? 0.5 : 1,
                  height: "38px",
                  minWidth: "140px",
                }
              : { background: "var(--accent)", color: "#fff", height: "38px", minWidth: "140px" }
          }
        >
          {embedButtonLabel()}
        </button>
      </div>
      {fonts.length > 0 && isSingleFile && (
        <p className="text-xs -mt-2" style={{ color: "var(--text-secondary)" }}>
          {t("fonts_full_embed_warning")}
        </p>
      )}

      {/* Detection grid — single-file only. In batch mode, replaced by a
           placeholder describing what happens at embed time. */}
      {isBatch ? (
        <div
          className="rounded-lg px-4 py-6 text-center"
          style={{
            border: "1px solid var(--border)",
            background: "var(--bg-panel)",
          }}
        >
          <p className="text-sm" style={{ color: "var(--text-primary)" }}>
            {t("fonts_batch_placeholder_title", fileCount)}
          </p>
          <p className="text-xs mt-1" style={{ color: "var(--text-secondary)" }}>
            {t("fonts_batch_placeholder_body")}
          </p>
        </div>
      ) : (
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
                {fonts.map((info) => {
                  const selKey = fontSelectionKey(info);
                  return (
                    <label key={selKey} className={"font-row" + (!info.filePath ? " missing" : "")}>
                      <input
                        type="checkbox"
                        id={`font-row-${selKey}`}
                        name={`font-${selKey}`}
                        checked={selected.has(selKey)}
                        onChange={() => toggleSelect(selKey)}
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
                  );
                })}
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
      )}

      {/* Per-font subsetting progress (single-file only — batch uses
           the footer N-of-M chip). */}
      {progress && isSingleFile && (
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

      {/* Font source modal — uses first file's usages for coverage stats
           in single-file mode; in batch the modal still shows but the
           coverage is for file #1 only (representative sample). */}
      <FontSourceModal
        open={sourceModalOpen}
        onClose={() => setSourceModalOpen(false)}
        sources={fontSources}
        usages={fontUsages}
        userFontMap={userFontMap}
        hasSubtitle={fileCount > 0}
        onAddSource={handleAddFontSource}
        onRemoveSource={handleRemoveFontSource}
      />
    </div>
  );
}

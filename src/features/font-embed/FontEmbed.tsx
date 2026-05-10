import { useState, useCallback, useEffect, useRef, useMemo } from "react";
import {
  clearFontSources,
  fileNameFromPath,
  openFontCache,
  pickAssFiles,
  readText,
  removeFontSource,
  writeText,
} from "../../lib/tauri-api";
import { ask } from "@tauri-apps/plugin-dialog";
import {
  aggregateFonts,
  analyzeFonts,
  embedFonts,
  userFontKey,
  deriveEmbeddedPath,
  type FileAnalysis,
  type FontInfo,
  type EmbedProgress,
  type SystemFontResolution,
} from "./font-embedder";
import { ensureLoaded, fontKeyLabel, type FontUsage } from "./font-collector";
import { useI18n } from "../../i18n/useI18n";
import { useFileContext } from "../../lib/FileContext";
import type { Status } from "../../lib/StatusContext";
import { useTabStatus } from "../../lib/useTabStatus";
import FontSourceModal, { type FontSource } from "./FontSourceModal";
import { useFolderDrop } from "../../lib/useFolderDrop";
import { countExistingFiles } from "../../lib/output-collisions";
import { useClickOutside } from "../../lib/useClickOutside";
import { useLogPanel } from "../../lib/useLogPanel";
import { LogPanel } from "../../lib/LogPanel";
import { DropErrorBanner } from "../../lib/DropErrorBanner";
import { buildConflictMessage, normalizeOutputKey } from "../../lib/dedup-helpers";

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
  // analyzed during the embed loop using the Rust-owned local font-source
  // index.
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
  const { logs, addLog, clearLogs, logScrollRef } = useLogPanel();
  const [showFileList, setShowFileList] = useState(false);
  const [dropActive, setDropActive] = useState(false);
  const [dropError, setDropError] = useState<string | null>(null);

  const abortRef = useRef<AbortController | null>(null);
  // Synchronous double-click guard — `embedding` state lags
  // setEmbedding(true) by one render. busyRef is written synchronously
  // at handler entry and released in the outer finally so every exit
  // path clears it.
  const busyRef = useRef(false);
  // Generation counter for ingest flows. Each handlePickFiles /
  // handleDroppedPaths / handleClearFiles bumps the counter, and the
  // async work captured the value at entry — when it later checks
  // `gen !== pickGenRef.current`, a stale generation means the user
  // has since picked another batch (or cleared) and we abandon the
  // outdated work without writing state. Standard "discard stale
  // async results" idiom — see the same shape in BatchRename.
  const pickGenRef = useRef(0);
  // Per-file analysis cache: <path → {content, infos, usages}>. Holds
  // all batch contents in memory so the unified detection grid + the
  // embed loop don't have to re-read or re-parse on every interaction.
  // Source-change reanalysis runs against this map; embed reads from
  // it directly. Ref instead of state — UI consumes the AGGREGATE
  // (fonts / fontUsages state below), not the cache directly.
  const perFileAnalysisRef = useRef<Map<string, FileAnalysis>>(new Map());
  const dropZoneRef = useRef<HTMLDivElement>(null);
  const fileContainerRef = useRef<HTMLDivElement>(null);

  // Persistent cache availability — when init failed for a non-schema
  // reason (disk full, permissions denied), the App-level launch hook
  // logs WARN to stderr but a GUI app has no visible stderr. Surface
  // the state inline here so users see why embed is silently using
  // system fonts only. Schema-mismatch is handled by the launch-time
  // modal and is NOT shown as a banner (avoids double-surfacing).
  const [cacheUnavailable, setCacheUnavailable] = useState(false);
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const status = await openFontCache();
        if (cancelled) return;
        setCacheUnavailable(!status.available && !status.schemaMismatch);
      } catch {
        if (!cancelled) setCacheUnavailable(true);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // ── Local font sources (persist for the tab session) ─────
  const [fontSources, setFontSources] = useState<FontSource[]>([]);
  const [sourceModalOpen, setSourceModalOpen] = useState(false);
  // Transient busy flag during clear/remove font-source flows. Each
  // handler does an IPC call followed by reanalyzeWithSources(); without
  // a busy lock the UI shows the cleared state while the per-file
  // analysis cache is still rebuilding, and a fast double-click could
  // launch overlapping rebuilds. The lock disables the affected buttons
  // for the duration so the visual state matches what the IPC layer is
  // actually doing.
  const [sourceBusy, setSourceBusy] = useState(false);
  // The modal lifts its scanning state up here so the parent ✕ Clear
  // button can join the same lock. Without this, a user with the modal
  // mid-scan could click the parent ✕ Clear and trip Rust's
  // `reject_during_active_scan` guard, surfacing as a generic IPC
  // error log rather than a disabled button.
  const [modalScanning, setModalScanning] = useState(false);
  // Effective lock for any UI element that mutates font sources from
  // outside the modal: own clear/remove/add operations OR the modal's
  // active scan.
  const sourceLocked = sourceBusy || modalScanning;

  const fontSourceEntryCount = useMemo(
    () => fontSources.reduce((sum, src) => sum + src.count, 0),
    [fontSources]
  );
  // Identity-not-content memo by design: setFonts always rebuilds the
  // array (analyzeFonts returns a fresh `infos`), so React's referential
  // === check is sufficient — no need to hash the contents to avoid
  // spurious recomputes. If a future change ever mutates `fonts` in
  // place (it shouldn't), this would over-cache; but in-place mutation
  // would break far more than this memo.
  const localCoveredKeys = useMemo(() => {
    const out = new Set<string>();
    for (const info of fonts) {
      if (info.source === "local") out.add(fontSelectionKey(info));
    }
    return out;
  }, [fonts]);

  const filePaths = useMemo(() => fontsFiles?.filePaths ?? [], [fontsFiles]);
  const fileNames = useMemo(() => fontsFiles?.fileNames ?? [], [fontsFiles]);
  const primaryFileName = fileNames[0] ?? "";
  const fileCount = filePaths.length;
  const isSingleFile = fileCount === 1;
  const isBatch = fileCount > 1;

  // Shared ingestion path. Loads + analyzes EVERY file in the selection
  // upfront so the unified detection grid can show real coverage across
  // the whole batch (single-file is just N=1 of this code path). Cached
  // contents stay in memory for source-change reanalysis and the embed
  // loop. Sequential analyze keeps findSystemFont IPC pressure bounded.
  // Strict cross-tab dedup contract: any conflict rejects the WHOLE
  // selection — see buildConflictMessage / FileContext for rationale.
  const ingestPaths = useCallback(
    async (paths: string[], gen: number) => {
      const conflictMsg = buildConflictMessage(paths, "fonts", isFileInUse, t);
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

        const cache = new Map<string, FileAnalysis>();
        // Shared system-font cache for the whole batch — without this
        // every file repeats the same findSystemFont IPC for the same
        // missing font names, blowing up the Rust log with N×M warnings
        // and adding pointless latency to ingest. One cache per ingest
        // run keeps it scoped and disposable.
        const sysCache = new Map<string, SystemFontResolution>();
        // Symmetric cache for the persistent font cache lookup tier
        // (#5). Same N×M IPC concern as sysCache.
        const cacheLookupCache = new Map<string, { path: string; index: number } | null>();
        for (const path of paths) {
          if (gen !== pickGenRef.current) return;
          let content: string;
          try {
            content = await readText(path);
          } catch (e) {
            const reason = e instanceof Error ? e.message : String(e);
            addLog(t("msg_read_error", fileNameFromPath(path), reason), "error");
            continue;
          }
          if (gen !== pickGenRef.current) return;
          // Production path: ask Rust's session-local source index
          // for matches before falling back to system fonts. The
          // legacy in-memory userFontMap is null here because the
          // heavy index lives in Rust now.
          const useRustUserFonts = true;
          const analyzed = await analyzeFonts(
            content,
            null,
            sysCache,
            useRustUserFonts,
            cacheLookupCache
          );
          if (gen !== pickGenRef.current) return;
          cache.set(path, { content, infos: analyzed.infos, usages: analyzed.usages });
        }

        if (cache.size === 0) {
          // No file made it through (all reads failed). Don't enter a
          // half-loaded state.
          return;
        }

        perFileAnalysisRef.current = cache;
        const { infos: aggInfos, usages: aggUsages } = aggregateFonts(cache);
        setFontUsages(aggUsages);
        setFonts(aggInfos);
        setSelected(keysOfResolvedFonts(aggInfos));

        // Use the first SUCCESSFUL file's content for the
        // FontsFilesState.firstFileContent slot — the field is kept for
        // FileContext compatibility but the cache is the authoritative
        // store from here on.
        const firstSuccessfulPath = paths.find((p) => cache.has(p)) ?? paths[0];
        const firstContent = cache.get(firstSuccessfulPath)?.content ?? "";
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
    [isFileInUse, setFontsFiles, addLog, t]
  );

  const handlePickFiles = useCallback(async () => {
    const gen = (pickGenRef.current = pickGenRef.current + 1);
    const paths = await pickAssFiles(t);
    if (gen !== pickGenRef.current) return;
    if (!paths || paths.length === 0) return;
    await ingestPaths(paths, gen);
  }, [ingestPaths, t]);

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
    onError: (e) => setDropError(e instanceof Error ? e.message : String(e)),
    // Match the Pick-Files / Pick-Folder button gates which already
    // include `analyzing`. Drops accepted during analyze waste the
    // first analysis pass (gen counter saves correctness but the work
    // is thrown away) and produce a confusing "pick disabled, drop
    // accepted" UX.
    disabled: embedding || analyzing,
  });

  // ── Font source management ──────────────────────────────────────
  // Adding/removing a source: Rust owns the local font-source index; re-analyze
  // EVERY cached file's content against that fresh index so the unified
  // detection grid + per-file embed cache both reflect the new
  // resolution. Cached contents avoid disk re-reads.
  const reanalyzeWithSources = useCallback(async () => {
    const cache = perFileAnalysisRef.current;
    if (cache.size === 0) return;
    const gen = (pickGenRef.current = pickGenRef.current + 1);
    try {
      const newCache = new Map<string, FileAnalysis>();
      // Same batch-shared caches as ingestPaths — one round of
      // system / cache lookups per source change, not per (file × font).
      const sysCache = new Map<string, SystemFontResolution>();
      const cacheLookupCache = new Map<string, { path: string; index: number } | null>();
      for (const [path, prev] of cache) {
        if (gen !== pickGenRef.current) return;
        const analyzed = await analyzeFonts(prev.content, null, sysCache, true, cacheLookupCache);
        if (gen !== pickGenRef.current) return;
        newCache.set(path, {
          content: prev.content,
          infos: analyzed.infos,
          usages: analyzed.usages,
        });
      }
      perFileAnalysisRef.current = newCache;
      const { infos, usages } = aggregateFonts(newCache);
      setFontUsages(usages);
      setFonts(infos);
      // Merge: keep manual unchecks; auto-check newly resolved fonts.
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
  }, [addLog, t]);

  // Clear all font sources at once. Mirrors the file-strip ✕ pattern
  // — sources persist across subtitle clears by design, but the
  // separate ✕ on the source button gives the user one-click recovery
  // without diving into the modal.
  const handleClearFontSources = useCallback(() => {
    void (async () => {
      setSourceBusy(true);
      try {
        await clearFontSources();
        setFontSources([]);
        await reanalyzeWithSources();
      } catch (e) {
        addLog(t("error_prefix", e instanceof Error ? e.message : String(e)), "error");
      } finally {
        setSourceBusy(false);
      }
    })();
  }, [reanalyzeWithSources, addLog, t]);

  const handleAddFontSource = useCallback(
    (source: FontSource) => {
      const nextSources = [...fontSources, source];
      setFontSources(nextSources);
      // Wrap the reanalyze in the same sourceBusy envelope as
      // clear/remove. Without this, a user adding a source then
      // immediately clicking the parent ✕ Clear before the reanalyze
      // settles can fire a clear IPC concurrent with an in-flight
      // analyzeFonts pass against stale source state — visible Local
      // badges in the detection grid then disagree with the actually-
      // empty source list.
      void (async () => {
        setSourceBusy(true);
        try {
          await reanalyzeWithSources();
        } finally {
          setSourceBusy(false);
        }
      })();
    },
    [fontSources, reanalyzeWithSources]
  );

  const handleRemoveFontSource = useCallback(
    (id: string) => {
      void (async () => {
        setSourceBusy(true);
        try {
          await removeFontSource(id);
          const nextSources = fontSources.filter((s) => s.id !== id);
          setFontSources(nextSources);
          await reanalyzeWithSources();
        } catch (e) {
          addLog(t("error_prefix", e instanceof Error ? e.message : String(e)), "error");
        } finally {
          setSourceBusy(false);
        }
      })();
    },
    [fontSources, reanalyzeWithSources, addLog, t]
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
  useClickOutside(showFileList, fileContainerRef, () => setShowFileList(false));

  // ── Embed handler — handles both single-file and batch modes ─────
  const handleEmbed = useCallback(async () => {
    if (fileCount === 0) return;
    // Synchronous double-click gate — see HdrConvert::handleConvert.
    if (busyRef.current) return;
    busyRef.current = true;
    try {
      // Pre-flight overwrite check — same project-wide pattern.
      const projectedOutputs = filePaths.map((p) => deriveEmbeddedPath(p));
      try {
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
      } catch (e) {
        addLog(t("error_prefix", e instanceof Error ? e.message : String(e)), "error");
        setLastActionResult("error");
        return;
      }

      // Construct AbortController at the boundary into busy state — see
      // HdrConvert::handleConvert for rationale.
      abortRef.current = new AbortController();
      setEmbedding(true);
      setBatchProgress({ processed: 0, total: filePaths.length });

      try {
        addLog(t("msg_fonts_start", filePaths.length));

        let successCount = 0;
        let processedCount = 0;
        const seenOutputs = new Set<string>();

        for (let i = 0; i < filePaths.length; i++) {
          const filePath = filePaths[i];

          if (abortRef.current?.signal.aborted) {
            addLog(t("msg_fonts_cancelled"), "info");
            break;
          }

          const fileName = fileNameFromPath(filePath);
          addLog(t("msg_processing", fileName));

          try {
            const outputPath = deriveEmbeddedPath(filePath);
            const normalizedOut = normalizeOutputKey(outputPath);
            if (seenOutputs.has(normalizedOut)) {
              addLog(t("msg_skipped_duplicate", fileName), "error");
              continue;
            }
            seenOutputs.add(normalizedOut);

            // Pull from the per-file analysis cache populated at ingest.
            // The cache holds content, infos, and usages so the embed
            // loop avoids any disk re-read or re-analysis. Fall back to
            // a fresh read + analyze if cache somehow missed this path
            // (shouldn't happen — we ingest every file before showing
            // the grid — but defensive).
            let cached = perFileAnalysisRef.current.get(filePath);
            if (!cached) {
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
              if (abortRef.current?.signal.aborted) break;
              const analyzed = await analyzeFonts(content, null, undefined, true);
              cached = { content, infos: analyzed.infos, usages: analyzed.usages };
            }
            if (abortRef.current?.signal.aborted) break;

            // Filter to fonts THIS FILE references AND the user kept
            // checked in the global aggregate grid AND that resolved to
            // a real font file. The aggregate keys are the same shape
            // as per-file keys, so set membership is direct.
            //
            // `selected` is the Set captured when the user clicked Embed
            // (handleEmbed is a useCallback closing over the state value,
            // not a ref). Mid-run checkbox toggles do NOT affect what the
            // running batch embeds — that's intentional, so flipping a
            // box mid-loop can't inject or omit a font part-way through
            // the sequence and produce inconsistent outputs across files.
            // Switching this to a ref would change that contract.
            const selectedFonts = cached.infos.filter(
              (info) => selected.has(fontSelectionKey(info)) && info.filePath
            );

            if (selectedFonts.length === 0) {
              addLog(t("msg_no_fonts_selected"), "error");
              continue;
            }

            // Per-font subsetting progress — only in single-file. In
            // batch we suppress to avoid a noisy progress bar that resets
            // per file; the footer N-of-M chip is the file-level signal.
            const onProgress = isSingleFile ? (p: EmbedProgress) => setProgress(p) : undefined;

            const result = await embedFonts(
              cached.content,
              selectedFonts,
              cached.usages,
              onProgress,
              () => abortRef.current?.signal.aborted ?? false,
              t
            );

            if (result === null) {
              // Cancelled mid-embed for this file — break out of batch.
              break;
            }
            if (abortRef.current?.signal.aborted) break;

            const outName = fileNameFromPath(outputPath);
            if (result.embeddedCount === 0) {
              // Nothing was actually embedded — skip the write so we
              // don't produce a `.embedded.ass` that's identical to the
              // input and log "saved" for a no-op. Surface as a warning
              // so the user knows the file was processed but the output
              // would have been a copy of the source.
              addLog(t("msg_embed_no_change", outName), "info");
              continue;
            }
            await writeText(outputPath, result.content);
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

        if (!abortRef.current?.signal.aborted) {
          addLog(t("msg_fonts_complete", successCount, filePaths.length), "success");
        }

        // Cancel takes precedence over success/error.
        if (abortRef.current?.signal.aborted) {
          setLastActionResult("cancelled");
        } else {
          setLastActionResult(successCount > 0 ? "success" : "error");
        }
      } finally {
        setEmbedding(false);
        setBatchProgress(null);
        setProgress(null);
      }
    } finally {
      busyRef.current = false;
    }
  }, [fileCount, filePaths, isSingleFile, selected, addLog, t]);

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
    if (selected.size === 0) {
      return { kind: "pending", message: t("status_fonts_pick") };
    }
    if (isSingleFile) {
      return { kind: "pending", message: t("status_fonts_pending", selected.size) };
    }
    return {
      kind: "pending",
      message: t("status_fonts_batch_pending", selected.size, fileCount),
    };
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
    perFileAnalysisRef.current = new Map();
    // Explicit reset — previously relied on a downstream useEffect on
    // fontsFiles change to clear lastActionResult, which works today
    // but couples this clear to that effect's dep tracking. Reset here
    // so the status pill goes neutral the moment the user hits Clear.
    setLastActionResult(null);
  }, [clearFile]);

  // Now that the batch grid carries per-font checkboxes too, the
  // disabled rule is uniform: 0 selected = nothing to embed, single
  // or batch.
  // Include `analyzing` for symmetry with the Select-Files / Pick-Folder
  // gates below. Today the race window is closed by ingest sequencing
  // (selected only flips after analyze completes), but a future change
  // that publishes per-file would otherwise quietly let Embed fire on a
  // partially-analyzed batch.
  const isEmbedDisabled = embedding || analyzing || selected.size === 0 || fileCount === 0;

  function embedButtonLabel(): string {
    if (embedding) return t("btn_embedding");
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
            disabled={analyzing || embedding}
            className="flex-none px-3 rounded-lg text-lg font-bold transition-colors"
            style={{
              background: analyzing || embedding ? "var(--bg-input)" : "var(--cancel-bg)",
              color: analyzing || embedding ? "var(--text-muted)" : "var(--cancel-text)",
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
      <DropErrorBanner message={dropError} onDismiss={() => setDropError(null)} />

      {cacheUnavailable && (
        <p className="text-xs ml-1" role="status" style={{ color: "var(--text-secondary)" }}>
          {t("font_cache_unavailable_banner")}
        </p>
      )}

      {/* Drop hint when idle */}
      {fileCount === 0 && !dropError && (
        <p className="text-xs ml-1" style={{ color: "var(--text-muted)" }}>
          {t("fonts_drop_hint")}
        </p>
      )}

      {/* Action row: select fonts + embed + cancel.
           Font source picking is intentionally decoupled from subtitle
           loading — the user can pick fonts first OR a subtitle first
           in any order. Sources live in Rust's local font index, which
           ingestPaths consumes when subtitles arrive, and the modal's
           coverage stats already gracefully handle the no-subtitle
           case (shows `font_coverage_no_subtitle` hint inside). */}
      <div className="flex items-center gap-2">
        <button
          onClick={() => setSourceModalOpen(true)}
          disabled={embedding}
          className="px-5 rounded-lg font-medium text-sm transition-colors"
          style={
            embedding
              ? {
                  background: "var(--accent-disabled-bg)",
                  color: "var(--accent-disabled-text)",
                  height: "38px",
                }
              : { background: "var(--accent)", color: "#fff", height: "38px" }
          }
        >
          {fontSources.length > 0
            ? t("btn_select_font_files_with_count", fontSources.length)
            : t("btn_select_font_files")}
        </button>
        {fontSources.length > 0 && (
          <span
            className="min-w-0 truncate text-xs"
            style={{ color: "var(--text-secondary)" }}
            title={t("font_sources_loaded_summary", fontSourceEntryCount, fontSources.length)}
          >
            {t("font_sources_loaded_summary", fontSourceEntryCount, fontSources.length)}
          </span>
        )}
        {fontSources.length > 0 && (
          <button
            onClick={handleClearFontSources}
            disabled={embedding || sourceLocked}
            className="flex-none px-3 rounded-lg text-lg font-bold transition-colors"
            style={{
              background: embedding || sourceLocked ? "var(--bg-input)" : "var(--cancel-bg)",
              color: embedding || sourceLocked ? "var(--text-muted)" : "var(--cancel-text)",
              height: "38px",
            }}
            // Mirror the Modal's per-row remove tooltip pattern: when
            // disabled-by-busy, the tooltip should match the disabled
            // state instead of the action label that's not currently
            // available.
            title={sourceLocked ? t("font_sources_scanning") : t("btn_clear_font_sources")}
          >
            ✕
          </button>
        )}
        <div className="flex-1" />
        {embedding && (
          <button
            onClick={() => {
              abortRef.current?.abort();
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

      {/* Detection grid — same UI for single and batch. In batch the
           rows represent the UNION of unique fonts referenced anywhere
           in the selection; checkboxes act as a global filter applied
           per-file at embed time. */}
      <div
        className="rounded-lg"
        style={{
          border: "1px solid var(--border)",
          background: "var(--bg-panel)",
        }}
      >
        <div className="px-3 py-2" style={{ borderBottom: "1px solid var(--border)" }}>
          <span className="text-xs font-medium" style={{ color: "var(--text-muted)" }}>
            {fonts.length > 0
              ? isBatch
                ? t("fonts_title_count_batch", fonts.length, fileCount)
                : t("fonts_title_count", fonts.length)
              : t("fonts_title")}
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
                        {t(
                          info.source === "local"
                            ? "badge_local"
                            : info.source === "cache"
                              ? "badge_cache"
                              : "badge_system"
                        )}
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
                // numeric ratio of internal counts; safe by inspection.
                // eslint-disable-next-line no-restricted-syntax
                width: `${(progress.current / progress.total) * 100}%`,
              }}
            />
          </div>
        </div>
      )}

      {/* Log */}
      <LogPanel logs={logs} onClear={clearLogs} scrollRef={logScrollRef} />

      {/* Font source modal — uses first file's usages for coverage stats
           in single-file mode; in batch the modal still shows but the
           coverage is for file #1 only (representative sample). */}
      <FontSourceModal
        open={sourceModalOpen}
        onClose={() => setSourceModalOpen(false)}
        sources={fontSources}
        usages={fontUsages}
        localCoveredKeys={localCoveredKeys}
        hasSubtitle={fileCount > 0}
        onAddSource={handleAddFontSource}
        onRemoveSource={handleRemoveFontSource}
        onScanStateChange={setModalScanning}
      />
    </div>
  );
}

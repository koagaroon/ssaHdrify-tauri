import { useState, useCallback, useEffect, useRef, useMemo } from "react";
import {
  clearFontSources,
  fileNameFromPath,
  openFontCache,
  pickAssFiles,
  pickOutputDirectory,
  readText,
  removeFontSource,
  writeText,
} from "../../lib/tauri-api";
import { ask } from "@tauri-apps/plugin-dialog";
import {
  aggregateFonts,
  analyzeFonts,
  embedFonts,
  planEmbeddedOutputs,
  userFontKey,
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
import { buildConflictMessage, sanitizeError, sanitizeForDialog } from "../../lib/dedup-helpers";

type FontEmbedOutputMode = "beside_input" | "chosen_dir";

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

function missingReferencedFonts(infos: FontInfo[]): FontInfo[] {
  return infos.filter((info) => !info.filePath);
}

function safeDisplayFileName(path: string): string {
  try {
    return sanitizeForDialog(fileNameFromPath(path));
  } catch {
    return sanitizeForDialog(path.split(/[\\/]/).pop() ?? path);
  }
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

// Batch ingest caps — OOM-by-input defense.
//
// Font Embed is the only tab that retains EVERY batch file's decoded content
// in `perFileAnalysisRef` for the unified detection grid + embed loop —
// HDR / Time Shift / Batch Rename process per-file and discard. Without
// these caps, a 5000-file folder drop (the drag-drop expansion ceiling on
// the Rust side) × the 50 MB per-file encoding-layer cap = up to 250 GB
// theoretical retention. Realistic batches are season-scoped: ≤ 24 ASS
// files per pick; 500 leaves ample slack for full-series or multi-language
// batches without straying into adversarial territory.
//
// The aggregate-bytes cap is the load-bearing one — file count alone
// doesn't bound memory when individual files approach 50 MB. 500 MB
// covers any legitimate workflow (a typical fan-sub ASS is 30-200 KB,
// even rich-typography ones rarely exceed 5 MB).
const MAX_BATCH_FILES = 500;
// lowered from 500 MB to 200 MB headroom. The
// cap counts decoded UTF-16 string bytes (`content.length * 2`)
// only — per-file `usages` Maps with up to MAX_CODEPOINTS_PER_VARIANT
// entries × MAX_FONT_VARIANTS variants per file add another ~25-100
// MB of JS Set memory PER FILE under adversarial inputs (a fan-sub
// pack crafted to maximize codepoint diversity). At 500 MB
// content-bytes + N×100 MB Set retention, a 5-file batch could
// reach ~1 GB JS heap before the cap fires. 200 MB content-bytes
// keeps total batch retention under ~700 MB even under the worst-
// case Set inflation, comfortably inside V8's default heap. Real-
// world batches (24 ASS at 30-200 KB each) consume single-digit MB,
// so the lower cap is invisible to legitimate workflows.
const MAX_BATCH_AGGREGATE_BYTES = 200 * 1024 * 1024;

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
    "success" | "noop" | "partial" | "error" | "cancelled" | null
  >(null);
  // Synchronous "Cancelling…" feedback for the embed Cancel button. The abort
  // only lands at the next per-file iteration boundary, so without this the
  // user has no confirmation their click registered (mirrors FontSourceModal's
  // cancelRequested). Reset by the effect below when embedding settles.
  const [embedCancelRequested, setEmbedCancelRequested] = useState(false);
  const { logs, addLog, clearLogs, logScrollRef } = useLogPanel();
  const [showFileList, setShowFileList] = useState(false);
  const [dropActive, setDropActive] = useState(false);
  const [dropError, setDropError] = useState<string | null>(null);
  const [outputMode, setOutputMode] = useState<FontEmbedOutputMode>("beside_input");
  const [chosenOutputDir, setChosenOutputDir] = useState<string | null>(null);

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
  //
  // Per-component probe is intentional (vs lifting cacheStatus into a
  // Context shared with App.tsx): the banner needs to stay accurate
  // across the user's session — if they hit "Clear cache" in the drift
  // modal mid-session and clear succeeds-then-fails, this probe re-runs
  // when they next mount Font Embed. App.tsx's launch-time probe is
  // load-bearing for the modal; this one is load-bearing for the
  // banner. Two cheap SQL queries per launch is below the perf bar.
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
      // Defense-in-depth file-count cap . The drag-drop
      // expansion on the Rust side caps at 5000 entries; the multi-file
      // picker has no explicit cap. Reject up front so a malicious /
      // accidental large folder doesn't even start consuming readText
      // IPC bandwidth.
      if (paths.length > MAX_BATCH_FILES) {
        setDropError(t("err_batch_too_many_files", paths.length, MAX_BATCH_FILES));
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
        // Tracks total decoded-content bytes already retained in
        // `cache` so we abort before the next read pushes the batch
        // past MAX_BATCH_AGGREGATE_BYTES. UTF-16 string length × 2
        // approximates the JS-engine retention; the actual memory
        // footprint with `infos`/`usages` is somewhat higher, but the
        // dominant term is the decoded content (subtitles average
        // hundreds of bytes per Dialogue line; analysis structures
        // average a couple of bytes per font reference).
        let aggregateBytes = 0;
        for (const path of paths) {
          if (gen !== pickGenRef.current) return;
          let content: string;
          try {
            content = await readText(path);
          } catch (e) {
            // BiDi parity: read errors carry the file path
            // (often the user-picked path including filename), and
            // the Rust IPC error can interpolate that path again.
            const reason = sanitizeError(e);
            addLog(t("msg_read_error", sanitizeForDialog(fileNameFromPath(path)), reason), "error");
            continue;
          }
          if (gen !== pickGenRef.current) return;
          aggregateBytes += content.length * 2;
          if (aggregateBytes > MAX_BATCH_AGGREGATE_BYTES) {
            const mb = Math.round(aggregateBytes / (1024 * 1024));
            const capMb = Math.round(MAX_BATCH_AGGREGATE_BYTES / (1024 * 1024));
            setDropError(t("err_batch_aggregate_too_large", mb, capMb));
            return;
          }
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
        const firstSuccessfulPath = paths.find((p) => cache.has(p)) ?? paths[0]!;
        const firstContent = cache.get(firstSuccessfulPath)?.content ?? "";
        const names = paths.map(fileNameFromPath);
        setFontsFiles({
          filePaths: paths,
          fileNames: names,
          firstFileContent: firstContent,
        });
      } catch (e) {
        if (gen !== pickGenRef.current) return;
        addLog(t("error_prefix", sanitizeError(e)), "error");
      } finally {
        if (gen === pickGenRef.current) setAnalyzing(false);
      }
    },
    [isFileInUse, setFontsFiles, addLog, t]
  );

  const handlePickFiles = useCallback(async () => {
    const gen = (pickGenRef.current = pickGenRef.current + 1);
    // catch dialog IPC failures so a click
    // that can't open the picker doesn't read as a silent no-op. See
    // TimingShift.handlePickFiles for the full rationale.
    let paths: string[] | null;
    try {
      paths = await pickAssFiles(t);
    } catch (e) {
      if (gen !== pickGenRef.current) return;
      addLog(t("error_prefix", sanitizeError(e)), "error");
      return;
    }
    if (gen !== pickGenRef.current) return;
    if (!paths || paths.length === 0) return;
    await ingestPaths(paths, gen);
  }, [ingestPaths, addLog, t]);

  const handleDroppedPaths = useCallback(
    async (paths: string[]) => {
      // Bump pickGenRef at function entry, symmetric with
      // handlePickFiles. Previously the bump happened AFTER the
      // filter — concurrent in-flight pick wouldn't see the
      // generation jump until after the filter completed. Real-world
      // impact negligible (early-return is just log+banner state), but
      // the bump-at-entry pattern keeps the generation contract
      // uniform across pick entry points.
      const gen = (pickGenRef.current = pickGenRef.current + 1);
      // Clear lastActionResult on every new ingest entry, not just the
      // happy path. The `fontsFiles` useEffect that previously handled
      // this clear doesn't fire when the drop is rejected — leaving a
      // stale "Embed complete" footer alongside a fresh red banner.
      setLastActionResult(null);
      const assPaths = paths.filter((p) => fileNameHasAssExt(fileNameFromPath(p)));
      if (assPaths.length === 0) {
        // Pattern 1 sibling parity with HdrConvert.tsx + TimingShift.tsx:
        // surface through both the log AND the DropErrorBanner —
        // users with collapsed log panels see nothing from log-only.
        const msg = t("msg_no_subtitle_in_drop");
        addLog(msg, "error");
        setDropError(msg);
        return;
      }
      await ingestPaths(assPaths, gen);
    },
    [ingestPaths, addLog, setDropError, setLastActionResult, t]
  );

  useFolderDrop({
    ref: dropZoneRef,
    onPaths: handleDroppedPaths,
    onActiveChange: setDropActive,
    onError: (e) => setDropError(sanitizeError(e)),
    // Match the Pick-Files / Pick-Folder button gates which already
    // include `analyzing`. Drops accepted during analyze waste the
    // first analysis pass (gen counter saves correctness but the work
    // is thrown away) and produce a confusing "pick disabled, drop
    // accepted" UX.
    disabled: embedding || analyzing,
    t,
  });

  // ── Font source management ──────────────────────────────────────
  // Adding/removing a source: Rust owns the local font-source index; re-analyze
  // EVERY cached file's content against that fresh index so the unified
  // detection grid + per-file embed cache both reflect the new
  // resolution. Cached contents avoid disk re-reads.
  const reanalyzeWithSources = useCallback(async () => {
    const cache = perFileAnalysisRef.current;
    if (cache.size === 0) return;
    // snapshot the set of fonts that were ALREADY
    // resolved before this reanalyze. Below, the selection merge
    // distinguishes "truly new resolution" (auto-check) from "resolved
    // both before and after" (respect the user's prior selection
    // including manual unchecks). Without this snapshot, a font the
    // user manually unchecked gets auto-re-checked the next time
    // any source mutation triggers reanalyze — explicit user intent
    // silently overridden.
    const prevResolved = keysOfResolvedFonts(aggregateFonts(cache).infos);
    const gen = (pickGenRef.current = pickGenRef.current + 1);
    // set analyzing=true during the sequential
    // per-file IPC sequence. Without this, the Embed button remains
    // enabled while reanalyzeWithSources walks the cache against
    // (changed) sources — clicking Embed mid-reanalyze would consume
    // the stale `selected` set and skip fonts that the user just
    // unlocked by adding a source. Symmetric with ingestPaths above.
    setAnalyzing(true);
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
      // Merge: keep manual unchecks; auto-check ONLY truly new resolutions.
      // The auto-check loop gates on `!prevResolved.has(key)` so a font
      // that was resolved before AND manually unchecked stays unchecked.
      // The two-set diff (was-resolved vs now-resolved) is the precise
      // definition of "newly resolved"; checking only `!prev.has(key)`
      // would treat re-resolution identically to first-time resolution.
      setSelected((prev) => {
        const resolved = keysOfResolvedFonts(infos);
        const next = new Set<string>();
        for (const key of prev) {
          if (resolved.has(key)) next.add(key);
        }
        for (const key of resolved) {
          if (!prev.has(key) && !prevResolved.has(key)) next.add(key);
        }
        return next;
      });
    } catch (e) {
      if (gen !== pickGenRef.current) return;
      addLog(t("error_prefix", sanitizeError(e)), "error");
    } finally {
      // Generation-gated unlock (same pattern as ingestPaths): only
      // the latest reanalyze flips analyzing back off, so an in-flight
      // older run completing late doesn't briefly un-lock the UI
      // while a newer reanalyze is still mid-IPC.
      if (gen === pickGenRef.current) setAnalyzing(false);
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
        addLog(t("error_prefix", sanitizeError(e)), "error");
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
        } catch (e) {
          // parity with handleRemoveFontSource —
          // reanalyzeWithSources can throw (font_cache_commands /
          // analyzeFonts internal errors); without an outer catch the
          // rejection becomes an unhandled-promise warning and the user
          // sees no log entry explaining why the newly-added source
          // didn't update the detection grid.
          addLog(t("error_prefix", sanitizeError(e)), "error");
        } finally {
          setSourceBusy(false);
        }
      })();
    },
    [fontSources, reanalyzeWithSources, addLog, t]
  );

  const handleRemoveFontSource = useCallback(
    (id: string) => {
      void (async () => {
        // Look up the source's kind before the remove call — needed to
        // gate the persistent cache eviction . Defaults
        // to "files" (NOT "dir") when the source isn't found: dir-mode
        // removal triggers a `try_remove_folder_from_gui_cache` side
        // effect, and an unknown id falling into that branch could evict
        // an unrelated tracked folder if a previous source label
        // collided. files-mode never touches the persistent cache, so
        // it's the safe fallback for the (in-practice unreachable)
        // source-not-found path.
        const source = fontSources.find((s) => s.id === id);
        const kind = source?.kind ?? "files";
        setSourceBusy(true);
        try {
          await removeFontSource(id, kind);
          const nextSources = fontSources.filter((s) => s.id !== id);
          setFontSources(nextSources);
          await reanalyzeWithSources();
        } catch (e) {
          addLog(t("error_prefix", sanitizeError(e)), "error");
        } finally {
          setSourceBusy(false);
        }
      })();
    },
    [fontSources, reanalyzeWithSources, addLog, t]
  );

  const handlePickOutputDir = useCallback(async () => {
    if (analyzing || embedding) return;
    try {
      const dir = await pickOutputDirectory(t);
      if (dir) setChosenOutputDir(dir);
    } catch (e) {
      addLog(t("error_prefix", sanitizeError(e)), "error");
    }
  }, [analyzing, embedding, addLog, t]);

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

  // Output destination changes alter what a subsequent Embed click will do,
  // so clear stale "done" / "cancelled" status just like file changes do.
  useEffect(() => {
    setLastActionResult(null);
  }, [outputMode, chosenOutputDir]);

  // File-list dropdown: close on click-outside / Escape (mirrors HDR).
  useClickOutside(showFileList, fileContainerRef, () => setShowFileList(false));

  // ── Embed handler — handles both single-file and batch modes ─────
  const handleEmbed = useCallback(async () => {
    if (fileCount === 0) return;
    // Synchronous double-click gate — see HdrConvert::handleConvert.
    if (busyRef.current) return;
    busyRef.current = true;
    try {
      if (outputMode === "chosen_dir" && !chosenOutputDir) {
        addLog(t("msg_rename_no_chosen_dir"), "error");
        setLastActionResult("error");
        return;
      }

      const planned = planEmbeddedOutputs(
        filePaths,
        outputMode === "chosen_dir" ? { outputDir: chosenOutputDir! } : {}
      );
      for (const skipped of planned.skipped) {
        const safePath = safeDisplayFileName(skipped.inputPath);
        if (skipped.reason === "duplicate") {
          addLog(t("msg_skipped_duplicate", safePath), "error");
        } else {
          addLog(t("msg_fonts_error", safePath, skipped.message), "error");
        }
      }
      if (planned.targets.length === 0) {
        // Every file failed pre-flight — nothing left to process.
        setLastActionResult("error");
        return;
      }
      try {
        const existingCount = await countExistingFiles(
          planned.targets.map((target) => target.outputPath)
        );
        if (existingCount > 0) {
          const skippedSuffix =
            planned.skipped.length > 0
              ? `\n\n${t("msg_fonts_skipped_count", planned.skipped.length)}`
              : "";
          const confirmed = await ask(
            t("msg_overwrite_confirm", existingCount, planned.targets.length) + skippedSuffix,
            {
              title: t("dialog_overwrite_title"),
              kind: "warning",
            }
          );
          if (!confirmed) {
            addLog(t("msg_fonts_cancelled"), "info");
            setLastActionResult("cancelled");
            return;
          }
        }
      } catch (e) {
        addLog(t("error_prefix", sanitizeError(e)), "error");
        setLastActionResult("error");
        return;
      }

      // Construct AbortController at the boundary into busy state — see
      // HdrConvert::handleConvert for rationale.
      abortRef.current = new AbortController();
      setEmbedding(true);
      setBatchProgress({ processed: planned.skipped.length, total: filePaths.length });

      try {
        addLog(t("msg_fonts_start", filePaths.length));

        let successCount = 0;
        let issueCount = planned.skipped.length;
        // Files that processed cleanly but needed no embedding (every
        // referenced font already present). Tracked apart from successCount /
        // issueCount so an all-no-change batch isn't misframed as total
        // failure in the final summary.
        let noChangeCount = 0;
        let processedCount = planned.skipped.length;

        for (const target of planned.targets) {
          const filePath = target.inputPath;
          const outputPath = target.outputPath;

          // Compute once at the top of each loop so every downstream
          // log interpolation gets the same BiDi-scrubbed display name.
          const safeFileName = safeDisplayFileName(filePath);

          if (abortRef.current?.signal.aborted) {
            addLog(t("msg_fonts_cancelled"), "info");
            break;
          }

          // BiDi parity — fileName / outName below flow into many
          // addLog calls. The sanitized form is computed once at
          // source (hoisted above) so every downstream interpolation
          // site automatically gets BiDi-scrubbed display text
          // without each callsite having to remember to wrap.
          addLog(t("msg_processing", safeFileName));

          try {
            // Pull from the per-file analysis cache populated at ingest.
            // Every visible path in the grid was ingested before this
            // loop runs (see `ingestPaths`), and ingest is the site that
            // enforces MAX_BATCH_AGGREGATE_BYTES. The previous defensive
            // fresh-read fallback bypassed that cap; a cache miss at
            // this point is a real inconsistency that should surface
            // as an error rather than silently re-read outside the
            // ingest guards.
            const cached = perFileAnalysisRef.current.get(filePath);
            if (!cached) {
              issueCount++;
              addLog(t("msg_fonts_error", safeFileName, "analysis cache miss"), "error");
              continue;
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
            const missingFonts = missingReferencedFonts(cached.infos);
            const missingWarnings =
              missingFonts.length > 0 ? [t("msg_fonts_missing_warning", missingFonts.length)] : [];

            if (selectedFonts.length === 0) {
              // misattributed message correction.
              // The outer "Embed" button is gated on `selected.size > 0`,
              // so the global-zero case never reaches this loop body.
              // The remaining 0-fonts case here is "this file references
              // no fonts that the user kept checked" — a per-file,
              // per-batch shape that needs its own message naming the
              // file. `safeFileName` is already sanitized just above.
              for (const warning of missingWarnings) {
                addLog(t("msg_fonts_file_warning", safeFileName, warning), "warn");
              }
              // Count this file as ONE issue ("no usable fonts"). The
              // per-missing-font warnings above are logged for detail but must
              // not ALSO be summed here, or one file inflates the summary's
              // issue count past the file count.
              issueCount++;
              addLog(t("msg_no_fonts_for_file", safeFileName), "error");
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

            // outName goes through addLog twice below; sanitize once
            // at derivation. fileNameFromPath result is from a
            // user-derived path that may carry P1b attacker-influenced
            // segments.
            const safeOutName = sanitizeForDialog(fileNameFromPath(outputPath));
            if (result.embeddedCount === 0) {
              const fileWarnings = [...missingWarnings, ...result.warnings];
              if (fileWarnings.length > 0) {
                issueCount += fileWarnings.length;
                for (const warning of fileWarnings) {
                  addLog(t("msg_fonts_file_warning", safeFileName, warning), "warn");
                }
              } else {
                // Benign no-change: every referenced font is already present,
                // so there was nothing to embed. Not a failure — tracked
                // separately so an all-no-change batch isn't routed to
                // msg_fonts_all_failed below.
                noChangeCount++;
              }
              // Nothing was actually embedded — skip the write so we
              // don't produce a `.embedded.ass` that's identical to the
              // input and log "saved" for a no-op. Surface as a warning
              // so the user knows the file was processed but the output
              // would have been a copy of the source.
              addLog(
                t("msg_embed_no_change", safeOutName),
                fileWarnings.length > 0 ? "warn" : "info"
              );
              continue;
            }
            await writeText(outputPath, result.content);
            const fileWarnings = [...missingWarnings, ...result.warnings];
            if (fileWarnings.length > 0) {
              issueCount += fileWarnings.length;
              for (const warning of fileWarnings) {
                addLog(t("msg_fonts_file_warning", safeFileName, warning), "warn");
              }
              addLog(
                t(
                  "msg_embed_saved_partial",
                  safeOutName,
                  result.embeddedCount,
                  fileWarnings.length
                ),
                "warn"
              );
            } else {
              addLog(t("msg_embed_saved", safeOutName, result.embeddedCount), "success");
            }
            successCount++;
          } catch (e) {
            issueCount++;
            addLog(t("msg_fonts_error", safeFileName, sanitizeError(e)), "error");
          } finally {
            processedCount++;
            setBatchProgress({ processed: processedCount, total: filePaths.length });
          }
        }

        // Match HDR/Timing post-loop pattern: gate the success log on
        // `successCount > 0`, route 0-success batches to a distinct
        // `msg_fonts_all_failed` line. Previously the success log
        // fired even on all-failure batches ("Embed complete: 0/N
        // processed" alongside a red error footer), contradicting
        // itself. HDR + Timing already correctly split these two
        // cases — bringing FontEmbed in line.
        const aborted = !!abortRef.current?.signal.aborted;
        if (aborted) {
          setLastActionResult("cancelled");
        } else if (successCount > 0 && issueCount > 0 && noChangeCount > 0) {
          addLog(
            t("msg_fonts_complete_partial_mixed", successCount, noChangeCount, issueCount),
            "warn"
          );
          setLastActionResult("partial");
        } else if (successCount > 0 && issueCount > 0) {
          addLog(
            t("msg_fonts_complete_partial", successCount, filePaths.length, issueCount),
            "warn"
          );
          setLastActionResult("partial");
        } else if (successCount > 0 && noChangeCount > 0) {
          addLog(t("msg_fonts_complete_mixed", successCount, noChangeCount), "success");
          setLastActionResult("success");
        } else if (successCount > 0) {
          addLog(t("msg_fonts_complete", successCount, filePaths.length), "success");
          setLastActionResult("success");
        } else if (issueCount === 0 && noChangeCount > 0) {
          // Every processed file already had all its referenced fonts —
          // nothing to embed. A benign outcome, not a failure: don't route it
          // to msg_fonts_all_failed.
          addLog(t("msg_fonts_all_no_change", noChangeCount), "info");
          setLastActionResult("noop");
        } else if (noChangeCount > 0 && issueCount > 0) {
          // Some files were benign no-ops while others failed preflight
          // or processing. That is incomplete, not "all failed".
          addLog(
            t("msg_fonts_complete_partial_mixed", successCount, noChangeCount, issueCount),
            "warn"
          );
          setLastActionResult("partial");
        } else {
          addLog(t("msg_fonts_all_failed", filePaths.length), "error");
          setLastActionResult("error");
        }
      } finally {
        setEmbedding(false);
        setBatchProgress(null);
        setProgress(null);
        setEmbedCancelRequested(false);
      }
    } finally {
      busyRef.current = false;
    }
  }, [fileCount, filePaths, isSingleFile, selected, outputMode, chosenOutputDir, addLog, t]);

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
    if (lastActionResult === "noop") return { kind: "done", message: t("status_fonts_noop") };
    if (lastActionResult === "partial") {
      return { kind: "pending", message: t("status_fonts_partial") };
    }
    if (lastActionResult === "error") return { kind: "error", message: t("status_fonts_error") };
    if (lastActionResult === "cancelled") {
      return { kind: "pending", message: t("status_fonts_cancelled") };
    }
    if (selected.size === 0) {
      return { kind: "pending", message: t("status_fonts_pick") };
    }
    if (missingChosenOutputDir) {
      return { kind: "pending", message: t("msg_rename_no_chosen_dir") };
    }
    if (isSingleFile) {
      return { kind: "pending", message: t("status_fonts_pending", selected.size) };
    }
    return {
      kind: "pending",
      message: t("status_fonts_batch_pending", selected.size, fileCount),
    };
  }, [
    fileCount,
    isSingleFile,
    analyzing,
    embedding,
    batchProgress,
    selected,
    lastActionResult,
    missingChosenOutputDir,
    t,
  ]);
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
  const outputControlsDisabled = analyzing || embedding;
  const missingChosenOutputDir = outputMode === "chosen_dir" && !chosenOutputDir;
  const isEmbedDisabled =
    embedding || analyzing || selected.size === 0 || fileCount === 0 || missingChosenOutputDir;

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

      <div className="flex flex-wrap items-center gap-3">
        <label
          className="flex items-center gap-2 text-sm cursor-pointer"
          style={{ color: "var(--text-primary)" }}
        >
          <input
            type="radio"
            name="font-embed-output-mode"
            value="beside_input"
            checked={outputMode === "beside_input"}
            onChange={() => setOutputMode("beside_input")}
            disabled={outputControlsDisabled}
          />
          <span>{t("fonts_output_beside_input")}</span>
          <span className="text-xs" style={{ color: "var(--text-muted)" }}>
            {t("rename_mode_default")}
          </span>
        </label>
        <label
          className="flex items-center gap-2 text-sm cursor-pointer"
          style={{ color: "var(--text-primary)" }}
        >
          <input
            type="radio"
            name="font-embed-output-mode"
            value="chosen_dir"
            checked={outputMode === "chosen_dir"}
            onChange={() => setOutputMode("chosen_dir")}
            disabled={outputControlsDisabled}
          />
          <span>{t("fonts_output_chosen_dir")}</span>
        </label>
        {outputMode === "chosen_dir" && (
          <div className="flex items-center gap-2 min-w-0">
            <button
              onClick={handlePickOutputDir}
              disabled={outputControlsDisabled}
              className="px-3 py-1 rounded text-xs font-medium"
              style={{
                background: outputControlsDisabled ? "var(--bg-input)" : "var(--accent)",
                color: outputControlsDisabled ? "var(--text-muted)" : "white",
              }}
            >
              {t("btn_pick_chosen_dir")}
            </button>
            {chosenOutputDir ? (
              <span
                className="text-xs truncate"
                style={{ color: "var(--text-secondary)", maxWidth: "min(420px, 55vw)" }}
                title={sanitizeForDialog(chosenOutputDir)}
              >
                {sanitizeForDialog(chosenOutputDir)}
              </span>
            ) : (
              <span className="text-xs italic" style={{ color: "var(--text-muted)" }}>
                {t("rename_chosen_dir_empty")}
              </span>
            )}
          </div>
        )}
      </div>

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
              setEmbedCancelRequested(true);
              abortRef.current?.abort();
            }}
            disabled={embedCancelRequested}
            className="px-4 rounded-lg text-sm transition-colors"
            style={{
              background: "var(--cancel-bg)",
              color: "var(--cancel-text)",
              height: "38px",
              filter: embedCancelRequested ? "grayscale(1)" : "none",
            }}
          >
            {embedCancelRequested ? t("btn_cancelling") : t("btn_cancel")}
          </button>
        )}
        <button
          onClick={handleEmbed}
          disabled={isEmbedDisabled}
          title={missingChosenOutputDir ? t("msg_rename_no_chosen_dir") : undefined}
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
                // `progress.total || 1` guards against a future refactor
                // that publishes a progress event before computing total
                // — current loop short-circuits before the embed call so
                // total is always > 0, but the divide-by-zero NaN would
                // render `NaN%` and CSS-fail silently.
                // eslint-disable-next-line no-restricted-syntax
                width: `${(progress.current / (progress.total || 1)) * 100}%`,
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

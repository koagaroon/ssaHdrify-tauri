import { useState, useCallback, useEffect, useRef, useMemo } from "react";
import {
  fileNameFromPath,
  textDecodingWarnings,
  pickSubtitleFiles,
  pickTimingMapFile,
  readText,
  readTimingMapFile,
  writeText,
} from "../../lib/tauri-api";
import { ask } from "@tauri-apps/plugin-dialog";
import {
  previewShiftSubtitles,
  previewTimingMapSubtitles,
  shiftSubtitlesCompact,
  shiftSubtitlesWithTimingMapCompact,
  parseTimingMapText,
  hasEnabledTimingMapRules,
  formatDisplayTime,
  parseDisplayTime,
  deriveShiftedPath,
  type PreviewEntry,
  type TimingMapPreviewEntry,
  type TimingMapRule,
} from "./timing-engine";
import { useI18n } from "../../i18n/useI18n";
import { useFileContext } from "../../lib/FileContext";
import type { Status } from "../../lib/StatusContext";
import { useTabStatus } from "../../lib/useTabStatus";
import { useFolderDrop } from "../../lib/useFolderDrop";
import { findExistingOutputKeys } from "../../lib/output-collisions";
import { assertOutputDoesNotReplaceSelectedInput } from "../../lib/path-validation";
import { useClickOutside } from "../../lib/useClickOutside";
import { useLogPanel } from "../../lib/useLogPanel";
import {
  formatSignedOffsetMs,
  formatTimingOffsetMagnitude,
  isTimingOffsetInvalid,
  isTimingSaveDisabled,
  parseTimingOffsetMagnitudeMs,
  type TimingShiftMode,
} from "./timing-ui-state";
import {
  buildConflictMessage,
  normalizeOutputKey,
  sanitizeError,
  sanitizeForDialog,
} from "../../lib/dedup-helpers";
import { categorize } from "../../lib/rename-extensions";

export type Unit = "ms" | "s";
export type Direction = "slower" | "faster";

/** Cap to ±1 year to prevent integer precision loss for extreme inputs. */
const MAX_OFFSET_MS = 365 * 24 * 3600 * 1000;

export const PREVIEW_ROW_HEIGHT = 30;
const PREVIEW_LIST_MAX_HEIGHT = 280;

export type AnyTimingPreviewEntry = PreviewEntry | TimingMapPreviewEntry;

export interface PreviewRowData {
  preview: AnyTimingPreviewEntry[];
  formatTime: (ms: number) => string;
  formatMatchedRule: (entry: AnyTimingPreviewEntry) => string | null;
  origLabel: string;
  shiftedLabel: string;
}

export interface LoadedTimingMapState {
  filePath: string;
  fileName: string;
  rules: TimingMapRule[];
  activeRuleCount: number;
}

interface TimingPreviewState {
  preview: AnyTimingPreviewEntry[];
  captionCount: number;
  shiftableCount: number;
  maxCaptionStart: number;
  detectedFormat: string;
  previewTruncated: boolean;
  error: string | null;
}

function emptyTimingPreviewState(error: string | null = null): TimingPreviewState {
  return {
    preview: [],
    captionCount: 0,
    shiftableCount: 0,
    maxCaptionStart: 0,
    detectedFormat: "",
    previewTruncated: false,
    error,
  };
}

export function useTimingShiftController() {
  const { t } = useI18n();
  const { timingFiles, setTimingFiles, clearFile, isFileInUse, findLoadedInputConflictKeys } =
    useFileContext();

  const [mode, setMode] = useState<TimingShiftMode>("offset");
  const [timingMap, setTimingMap] = useState<LoadedTimingMapState | null>(null);
  const [timingMapError, setTimingMapError] = useState<string | null>(null);

  const [previewState, setPreviewState] = useState<TimingPreviewState>(() =>
    emptyTimingPreviewState()
  );
  const {
    preview,
    captionCount,
    shiftableCount,
    maxCaptionStart,
    detectedFormat,
    previewTruncated,
    error: previewError,
  } = previewState;

  const [offsetMagnitudeMs, setOffsetMagnitudeMs] = useState(200);
  const [offsetText, setOffsetText] = useState("200");
  const [unit, setUnit] = useState<Unit>("ms");
  const [direction, setDirection] = useState<Direction>("slower");
  const [useThreshold, setUseThreshold] = useState(false);
  const [thresholdText, setThresholdText] = useState("00:05:00.000");
  const [busy, setBusy] = useState(false);
  const [lastActionResult, setLastActionResult] = useState<
    "success" | "partial" | "error" | "cancelled" | null
  >(null);
  const [progress, setProgress] = useState<{ processed: number; total: number } | null>(null);
  const [dropActive, setDropActive] = useState(false);
  const { logs, addLog, clearLogs, logScrollRef } = useLogPanel();
  const [showFileList, setShowFileList] = useState(false);
  const [dropError, setDropError] = useState<string | null>(null);

  const debounceRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const pickGenRef = useRef(0);
  const mapPickGenRef = useRef(0);
  const abortRef = useRef<AbortController | null>(null);
  const busyRef = useRef(false);
  const dropZoneRef = useRef<HTMLDivElement>(null);
  const fileContainerRef = useRef<HTMLDivElement>(null);

  const effectiveOffsetMs = useMemo(() => {
    if (direction === "faster" && offsetMagnitudeMs > 0) return -offsetMagnitudeMs;
    return offsetMagnitudeMs;
  }, [offsetMagnitudeMs, direction]);

  const offsetMax = unit === "s" ? MAX_OFFSET_MS / 1000 : MAX_OFFSET_MS;

  const handleUnitChange = (newUnit: Unit) => {
    if (newUnit === unit) return;
    setOffsetText(formatTimingOffsetMagnitude(offsetMagnitudeMs, newUnit));
    setUnit(newUnit);
  };
  const handleOffsetChange = (value: string) => {
    setOffsetText(value);
    const magnitudeMs = parseTimingOffsetMagnitudeMs(value, unit, offsetMax);
    if (magnitudeMs !== null) setOffsetMagnitudeMs(magnitudeMs);
  };
  const offsetInvalid = isTimingOffsetInvalid(offsetText, unit, offsetMax);

  const thresholdMs = useMemo(
    () => (useThreshold ? parseDisplayTime(thresholdText) : null),
    [useThreshold, thresholdText]
  );
  const thresholdInvalid = useThreshold && thresholdMs === null;

  const thresholdExceedsFile =
    mode === "offset" &&
    useThreshold &&
    thresholdMs !== null &&
    captionCount > 0 &&
    thresholdMs > maxCaptionStart;

  const timingMapReady = timingMap !== null && timingMap.activeRuleCount > 0;

  const filePaths = useMemo(() => timingFiles?.filePaths ?? [], [timingFiles]);
  const fileNames = useMemo(() => timingFiles?.fileNames ?? [], [timingFiles]);
  const firstFileContent = timingFiles?.firstFileContent ?? "";
  const primaryFileName = fileNames[0] ?? "";
  const fileCount = filePaths.length;

  const previewListStyle = useMemo(
    () => ({
      height: Math.min(preview.length * PREVIEW_ROW_HEIGHT, PREVIEW_LIST_MAX_HEIGHT),
    }),
    [preview.length]
  );

  const formatMatchedRule = useCallback(
    (entry: AnyTimingPreviewEntry): string | null => {
      if (!("ruleIndex" in entry) || entry.ruleIndex === null) return null;
      const offsetDisplay = formatSignedOffsetMs(entry.appliedOffsetMs);
      if (entry.ruleLabel) {
        return t(
          "timing_map_matched_rule_labeled_tooltip",
          entry.ruleIndex + 1,
          sanitizeForDialog(entry.ruleLabel),
          offsetDisplay
        );
      }
      return t("timing_map_matched_rule_tooltip", entry.ruleIndex + 1, offsetDisplay);
    },
    [t]
  );

  const previewRowProps = useMemo<PreviewRowData>(
    () => ({
      preview,
      formatTime: formatDisplayTime,
      formatMatchedRule,
      origLabel: t("col_original"),
      shiftedLabel: t("col_shifted"),
    }),
    [preview, formatMatchedRule, t]
  );

  useEffect(() => {
    if (!firstFileContent) {
      setPreviewState(emptyTimingPreviewState());
      return;
    }
    if (mode === "map") {
      if (!timingMapReady || !timingMap) {
        if (debounceRef.current) clearTimeout(debounceRef.current);
        setPreviewState(emptyTimingPreviewState());
        return;
      }
      if (debounceRef.current) clearTimeout(debounceRef.current);
      debounceRef.current = setTimeout(() => {
        try {
          const result = previewTimingMapSubtitles(firstFileContent, {
            rules: timingMap.rules,
          });
          setPreviewState({
            preview: result.preview,
            captionCount: result.captionCount,
            shiftableCount: result.shiftableCount,
            maxCaptionStart: result.maxCaptionStart,
            detectedFormat: result.format.toUpperCase(),
            previewTruncated: result.previewTruncated,
            error: null,
          });
        } catch (error) {
          setPreviewState(emptyTimingPreviewState(t("timing_preview_error", sanitizeError(error))));
        }
      }, 200);
      return () => {
        if (debounceRef.current) clearTimeout(debounceRef.current);
      };
    }

    if (offsetInvalid || thresholdInvalid) {
      if (debounceRef.current) clearTimeout(debounceRef.current);
      setPreviewState(emptyTimingPreviewState());
      return;
    }
    if (debounceRef.current) clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(() => {
      try {
        const result = previewShiftSubtitles(firstFileContent, {
          offsetMs: effectiveOffsetMs,
          thresholdMs: thresholdMs ?? undefined,
        });
        setPreviewState({
          preview: result.preview,
          captionCount: result.captionCount,
          shiftableCount: result.shiftableCount,
          maxCaptionStart: result.maxCaptionStart,
          detectedFormat: result.format.toUpperCase(),
          previewTruncated: result.previewTruncated,
          error: null,
        });
      } catch (error) {
        setPreviewState(emptyTimingPreviewState(t("timing_preview_error", sanitizeError(error))));
      }
    }, 200);
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
  }, [
    firstFileContent,
    mode,
    timingMap,
    timingMapReady,
    effectiveOffsetMs,
    offsetInvalid,
    thresholdMs,
    thresholdInvalid,
    t,
  ]);

  useEffect(() => {
    setLastActionResult(null);
  }, [timingFiles, mode, timingMap, effectiveOffsetMs, useThreshold, thresholdMs]);

  useEffect(() => {
    return () => {
      abortRef.current?.abort();
    };
  }, []);

  useClickOutside(showFileList, fileContainerRef, () => setShowFileList(false));

  const tabStatus = useMemo<Status>(() => {
    if (fileCount === 0) return { kind: "idle", message: t("status_timing_idle") };
    if (busy) {
      return {
        kind: "busy",
        message: t("status_timing_busy"),
        progress: progress ?? undefined,
      };
    }
    if (lastActionResult === "success") return { kind: "done", message: t("status_timing_done") };
    if (lastActionResult === "partial")
      return { kind: "pending", message: t("status_timing_partial") };
    if (lastActionResult === "error") return { kind: "error", message: t("status_timing_error") };
    if (lastActionResult === "cancelled") {
      return { kind: "pending", message: t("status_timing_cancelled") };
    }
    if (mode === "map") {
      return {
        kind: "pending",
        message: timingMapReady ? t("status_timing_map_pending") : t("status_timing_map_pick"),
      };
    }
    return { kind: "pending", message: t("status_timing_pending") };
  }, [fileCount, busy, lastActionResult, progress, mode, timingMapReady, t]);
  useTabStatus("timing", tabStatus);

  const ingestPaths = useCallback(
    async (paths: string[], gen: number) => {
      const conflictMsg = buildConflictMessage(paths, "timing", isFileInUse, t);
      if (conflictMsg) {
        setDropError(conflictMsg);
        return;
      }
      setDropError(null);

      let firstContent: string;
      try {
        firstContent = await readText(paths[0]!);
      } catch (e) {
        if (gen !== pickGenRef.current) return;
        addLog(t("error_prefix", sanitizeError(e)), "error");
        return;
      }
      if (gen !== pickGenRef.current) return;

      const names = paths.map(fileNameFromPath);
      setTimingFiles({
        filePaths: paths,
        fileNames: names,
        firstFileContent: firstContent,
      });
    },
    [isFileInUse, setTimingFiles, addLog, t]
  );

  const handlePickFiles = useCallback(async () => {
    if (busy || busyRef.current) return;
    const gen = (pickGenRef.current = pickGenRef.current + 1);
    let paths: string[] | null;
    try {
      paths = await pickSubtitleFiles(t);
    } catch (e) {
      if (gen !== pickGenRef.current) return;
      addLog(t("error_prefix", sanitizeError(e)), "error");
      return;
    }
    if (gen !== pickGenRef.current) return;
    if (!paths || paths.length === 0) return;
    await ingestPaths(paths, gen);
  }, [busy, ingestPaths, addLog, t]);

  const handleDroppedPaths = useCallback(
    async (paths: string[]) => {
      if (busy || busyRef.current) return;
      const gen = (pickGenRef.current = pickGenRef.current + 1);
      const subtitlePaths = paths.filter((p) => categorize(fileNameFromPath(p)) === "subtitle");
      if (subtitlePaths.length === 0) {
        const msg = t("msg_no_subtitle_in_drop");
        addLog(msg, "error");
        setDropError(msg);
        return;
      }
      await ingestPaths(subtitlePaths, gen);
    },
    [busy, ingestPaths, addLog, t]
  );

  useFolderDrop({
    ref: dropZoneRef,
    onPaths: handleDroppedPaths,
    onActiveChange: setDropActive,
    onError: (e) => setDropError(sanitizeError(e)),
    disabled: busy,
    t,
  });

  const handleClearFiles = useCallback(() => {
    if (busy || busyRef.current) return;
    pickGenRef.current = pickGenRef.current + 1;
    clearFile("timing");
    setPreviewState(emptyTimingPreviewState());
    setDropError(null);
    setLastActionResult(null);
  }, [busy, clearFile]);

  const handlePickTimingMap = useCallback(async () => {
    if (busy || busyRef.current) return;
    const gen = (mapPickGenRef.current = mapPickGenRef.current + 1);
    let mapPath: string | null;
    try {
      mapPath = await pickTimingMapFile(t);
    } catch (e) {
      if (gen !== mapPickGenRef.current) return;
      addLog(t("error_prefix", sanitizeError(e)), "error");
      return;
    }
    if (gen !== mapPickGenRef.current || !mapPath) return;

    let rawText: string;
    try {
      rawText = await readTimingMapFile(mapPath);
    } catch (e) {
      if (gen !== mapPickGenRef.current) return;
      const reason = sanitizeError(e);
      setTimingMap(null);
      setTimingMapError(reason);
      addLog(t("error_prefix", reason), "error");
      return;
    }
    if (gen !== mapPickGenRef.current) return;

    try {
      const parsed = parseTimingMapText(rawText);
      if (!hasEnabledTimingMapRules(parsed.rules)) {
        const msg = t("timing_map_no_enabled_rules");
        setTimingMap(null);
        setTimingMapError(msg);
        addLog(msg, "error");
        return;
      }
      const activeRuleCount = parsed.rules.filter((rule) => rule.enabled !== false).length;
      const safeName = sanitizeForDialog(fileNameFromPath(mapPath));
      setTimingMapError(null);
      setTimingMap({
        filePath: mapPath,
        fileName: safeName,
        rules: parsed.rules,
        activeRuleCount,
      });
      addLog(t("msg_timing_map_loaded", safeName, activeRuleCount, parsed.rules.length), "info");
    } catch (e) {
      const reason = sanitizeError(e);
      setTimingMap(null);
      setTimingMapError(reason);
      addLog(t("error_prefix", reason), "error");
    }
  }, [busy, addLog, t]);

  const handleClearTimingMap = useCallback(() => {
    if (busy || busyRef.current) return;
    mapPickGenRef.current = mapPickGenRef.current + 1;
    setTimingMap(null);
    setTimingMapError(null);
  }, [busy]);

  const handleSaveAll = useCallback(async () => {
    if (
      isTimingSaveDisabled({
        fileCount,
        thresholdInvalid,
        offsetInvalid,
        busy,
        mode,
        timingMapReady,
      })
    ) {
      return;
    }
    if (busyRef.current) return;
    const mapForRun = mode === "map" ? timingMap : null;
    if (mode === "map" && !mapForRun) return;

    busyRef.current = true;
    pickGenRef.current += 1;
    mapPickGenRef.current += 1;
    const controller = new AbortController();
    abortRef.current = controller;
    setBusy(true);
    try {
      const paths = filePaths;
      const selectedInputKeys = new Set(paths.map(normalizeOutputKey));

      const projectedOutputs: string[] = [];
      for (const filePath of paths) {
        try {
          const outputPath = deriveShiftedPath(filePath);
          assertOutputDoesNotReplaceSelectedInput(outputPath, selectedInputKeys);
          projectedOutputs.push(outputPath);
        } catch {
          // Re-thrown with per-file context inside the main loop below.
        }
      }
      let existingOutputs: ReadonlySet<string>;
      let conflictingOutputs: ReadonlySet<string>;
      try {
        conflictingOutputs = await findLoadedInputConflictKeys(projectedOutputs, controller.signal);
        if (controller.signal.aborted) {
          setLastActionResult("cancelled");
          return;
        }
        existingOutputs = await findExistingOutputKeys(
          projectedOutputs.filter((path) => !conflictingOutputs.has(normalizeOutputKey(path)))
        );
        if (controller.signal.aborted) {
          addLog(t("msg_timing_cancelled"), "info");
          setLastActionResult("cancelled");
          return;
        }
        const existingCount = existingOutputs.size;
        if (existingCount > 0) {
          const confirmed = await ask(t("msg_overwrite_confirm", existingCount, paths.length), {
            title: t("dialog_overwrite_title"),
            kind: "warning",
          });
          if (!confirmed || controller.signal.aborted) {
            addLog(t("msg_timing_cancelled"), "info");
            setLastActionResult("cancelled");
            return;
          }
        }
      } catch (e) {
        addLog(t("error_prefix", sanitizeError(e)), "error");
        setLastActionResult("error");
        return;
      }

      setProgress({ processed: 0, total: paths.length });

      try {
        if (mapForRun) {
          addLog(t("msg_timing_map_start", paths.length, mapForRun.activeRuleCount));
        } else {
          addLog(t("msg_timing_start", paths.length, effectiveOffsetMs));
        }

        let successCount = 0;
        let warnedCount = 0;
        let failedCount = 0;
        let processedCount = 0;
        const seenOutputs = new Set<string>();

        for (const filePath of paths) {
          if (abortRef.current?.signal.aborted) {
            addLog(t("msg_timing_cancelled"), "info");
            break;
          }

          let fileName = sanitizeForDialog(filePath.split(/[\\/]/).pop() ?? filePath);
          try {
            fileName = sanitizeForDialog(fileNameFromPath(filePath));
          } catch {
            // Keep basename fallback.
          }
          addLog(t("msg_processing", fileName));

          try {
            const outputPath = deriveShiftedPath(filePath);
            assertOutputDoesNotReplaceSelectedInput(outputPath, selectedInputKeys);
            assertOutputDoesNotReplaceSelectedInput(outputPath, conflictingOutputs);

            const normalizedOut = normalizeOutputKey(outputPath);
            if (seenOutputs.has(normalizedOut)) {
              failedCount++;
              addLog(t("msg_skipped_duplicate", fileName), "error");
              continue;
            }
            seenOutputs.add(normalizedOut);

            let content: string;
            let fileWarned = false;
            try {
              content = await readText(filePath, (read) => {
                const warnings = textDecodingWarnings(read, fileName, t);
                fileWarned = warnings.length > 0;
                for (const warning of warnings) addLog(warning, "warn");
              });
            } catch (e) {
              failedCount++;
              addLog(t("msg_read_error", fileName, sanitizeError(e)), "error");
              continue;
            }

            if (abortRef.current?.signal.aborted) break;

            const result = mapForRun
              ? shiftSubtitlesWithTimingMapCompact(content, { rules: mapForRun.rules })
              : shiftSubtitlesCompact(content, {
                  offsetMs: effectiveOffsetMs,
                  thresholdMs: thresholdMs ?? undefined,
                });

            if (result.skippedCount > 0) {
              fileWarned = true;
              const warningKey =
                result.format === "ass" || result.format === "vtt"
                  ? "msg_oversized_unchanged"
                  : "msg_oversized_skipped";
              addLog(t(warningKey, result.skippedCount, fileName), "warn");
            }

            if (abortRef.current?.signal.aborted) break;

            await writeText(outputPath, result.content, existingOutputs.has(normalizedOut));
            const outName = sanitizeForDialog(fileNameFromPath(outputPath));
            addLog(t("msg_saved", outName, result.captionCount), fileWarned ? "warn" : "success");
            if (fileWarned) warnedCount++;
            successCount++;
          } catch (e) {
            failedCount++;
            addLog(t("msg_timing_error", fileName, sanitizeError(e)), "error");
          } finally {
            processedCount++;
            setProgress({ processed: processedCount, total: paths.length });
          }
        }

        const aborted = !!abortRef.current?.signal.aborted;
        const issueCount = warnedCount + failedCount;
        if (aborted) {
          setLastActionResult("cancelled");
        } else if (successCount > 0) {
          if (issueCount > 0) {
            addLog(t("msg_complete_warnings", successCount, paths.length, issueCount), "warn");
            setLastActionResult("partial");
          } else {
            addLog(t("msg_timing_complete", successCount, paths.length), "success");
            setLastActionResult("success");
          }
        } else {
          addLog(t("msg_timing_all_failed", paths.length), "error");
          setLastActionResult("error");
        }
      } finally {
        setProgress(null);
      }
    } finally {
      abortRef.current = null;
      setBusy(false);
      busyRef.current = false;
    }
  }, [
    fileCount,
    filePaths,
    findLoadedInputConflictKeys,
    mode,
    timingMap,
    timingMapReady,
    effectiveOffsetMs,
    thresholdMs,
    thresholdInvalid,
    offsetInvalid,
    busy,
    addLog,
    t,
  ]);

  const requestCancel = useCallback(() => {
    abortRef.current?.abort();
  }, []);

  const dismissPreviewError = useCallback(() => {
    setPreviewState((state) => ({ ...state, error: null }));
  }, []);

  const saveDisabled = isTimingSaveDisabled({
    fileCount,
    thresholdInvalid,
    offsetInvalid,
    busy,
    mode,
    timingMapReady,
  });
  const saveLabel = fileCount > 1 ? t("btn_save_all", fileCount) : t("btn_save");

  return {
    t,
    mode,
    setMode,
    timingMap,
    timingMapError,
    setTimingMapError,
    handlePickTimingMap,
    handleClearTimingMap,
    filePaths,
    fileNames,
    primaryFileName,
    fileCount,
    showFileList,
    setShowFileList,
    dropZoneRef,
    fileContainerRef,
    dropActive,
    dropError,
    setDropError,
    preview,
    captionCount,
    shiftableCount,
    detectedFormat,
    previewTruncated,
    previewError,
    dismissPreviewError,
    previewListStyle,
    previewRowProps,
    offsetText,
    offsetMax,
    offsetInvalid,
    handleOffsetChange,
    unit,
    handleUnitChange,
    direction,
    setDirection,
    useThreshold,
    setUseThreshold,
    thresholdText,
    setThresholdText,
    thresholdInvalid,
    thresholdExceedsFile,
    busy,
    saveDisabled,
    saveLabel,
    handlePickFiles,
    handleClearFiles,
    handleSaveAll,
    requestCancel,
    logs,
    clearLogs,
    logScrollRef,
  };
}

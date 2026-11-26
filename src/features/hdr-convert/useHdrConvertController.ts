import { useState, useCallback, useEffect, useMemo, useRef } from "react";
import {
  fileNameFromPath,
  textDecodingWarnings,
  pickSubtitleFiles,
  readText,
  writeText,
} from "../../lib/tauri-api";
import { processAssContent } from "./ass-processor";
import {
  convertTextCueSubtitleToAss,
  isNativeAss,
  isConvertible,
  DEFAULT_STYLE,
  type StyleConfig,
} from "./srt-converter";
import { resolveOutputPath, DEFAULT_TEMPLATE } from "./output-naming";
import { DEFAULT_BRIGHTNESS, MIN_BRIGHTNESS, MAX_BRIGHTNESS, type Eotf } from "./color-engine";
import { ask } from "@tauri-apps/plugin-dialog";

import { parseFiniteNumberText } from "../../lib/strict-number";
import { isHdrBrightnessInvalid, isHdrConvertDisabled } from "./hdr-ui-state";
import {
  hasMicroDvdInput,
  isHdrFpsInvalid,
  parseHdrFpsOverrideForInput,
  type HdrFpsMode,
} from "./hdr-style-ui-state";
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
  buildConflictMessage,
  normalizeOutputKey,
  sanitizeError,
  sanitizeForDialog,
} from "../../lib/dedup-helpers";

export function useHdrConvertController() {
  const { t } = useI18n();
  const { hdrFiles, setHdrFiles, clearFile, isFileInUse, findLoadedInputConflictKeys } =
    useFileContext();
  const [eotf, setEotf] = useState<Eotf>("PQ");
  const [brightness, setBrightness] = useState(DEFAULT_BRIGHTNESS);
  const [brightnessText, setBrightnessText] = useState(String(DEFAULT_BRIGHTNESS));
  const [template, setTemplate] = useState<string>(DEFAULT_TEMPLATE);
  const [customTemplate, setCustomTemplate] = useState("");
  const [showStylePanel, setShowStylePanel] = useState(false);
  const [style, setStyle] = useState<StyleConfig>({ ...DEFAULT_STYLE });
  const [fpsMode, setFpsMode] = useState<HdrFpsMode>("auto");
  const [fpsText, setFpsText] = useState("23.976");
  const [processing, setProcessing] = useState(false);
  const { logs, addLog, clearLogs, logScrollRef } = useLogPanel();
  const [showFileList, setShowFileList] = useState(false);
  const [lastActionResult, setLastActionResult] = useState<
    "success" | "partial" | "error" | "cancelled" | null
  >(null);
  const [progress, setProgress] = useState<{ processed: number; total: number } | null>(null);
  const [dropActive, setDropActive] = useState(false);
  const [dropError, setDropError] = useState<string | null>(null);
  const abortRef = useRef<AbortController | null>(null);
  const busyRef = useRef(false);
  const pickGenRef = useRef(0);
  const fileContainerRef = useRef<HTMLDivElement>(null);
  const dropZoneRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    setLastActionResult(null);
  }, [hdrFiles]);

  const tabStatus = useMemo<Status>(() => {
    if (!hdrFiles) return { kind: "idle", message: t("status_hdr_idle") };
    if (processing) {
      return {
        kind: "busy",
        message: t("status_hdr_busy"),
        progress: progress ?? undefined,
      };
    }
    if (lastActionResult === "success") return { kind: "done", message: t("status_hdr_done") };
    if (lastActionResult === "partial")
      return { kind: "pending", message: t("status_hdr_partial") };
    if (lastActionResult === "error") return { kind: "error", message: t("status_hdr_error") };
    if (lastActionResult === "cancelled") {
      return { kind: "pending", message: t("status_hdr_cancelled") };
    }
    return {
      kind: "pending",
      message: t("status_hdr_pending", hdrFiles.filePaths.length),
    };
  }, [hdrFiles, processing, lastActionResult, progress, t]);
  useTabStatus("hdr", tabStatus);

  useClickOutside(showFileList, fileContainerRef, () => setShowFileList(false));

  const handleBrightnessChange = useCallback((value: string) => {
    setBrightnessText(value);
    const num = parseFiniteNumberText(value);
    if (num !== null && num >= MIN_BRIGHTNESS && num <= MAX_BRIGHTNESS) {
      setBrightness(Math.round(num));
    }
  }, []);

  const brightnessInvalid = isHdrBrightnessInvalid(brightnessText, MIN_BRIGHTNESS, MAX_BRIGHTNESS);

  const handleBrightnessFromNits = useCallback((nits: number) => {
    setBrightness(nits);
    setBrightnessText(String(nits));
  }, []);

  const activeTemplate = template === "custom" ? customTemplate : template;
  const templateInvalid = template === "custom" && customTemplate.trim().length === 0;
  const batchHasSub = hasMicroDvdInput(hdrFiles?.fileNames ?? []);
  const fpsInputInvalid = isHdrFpsInvalid(fpsMode, fpsText);
  const fpsInvalid = batchHasSub && fpsInputInvalid;
  const convertDisabled = isHdrConvertDisabled({
    hasFiles: !!hdrFiles,
    processing,
    brightnessInvalid,
    templateInvalid,
    fpsInvalid,
  });

  const handleSelectFiles = useCallback(async () => {
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

    const conflictMsg = buildConflictMessage(paths, "hdr", isFileInUse, t);
    if (conflictMsg) {
      setDropError(conflictMsg);
      return;
    }
    setDropError(null);

    const names = paths.map(fileNameFromPath);
    setHdrFiles({ filePaths: paths, fileNames: names });
  }, [isFileInUse, setHdrFiles, addLog, t]);

  const handleDroppedPaths = useCallback(
    (paths: string[]) => {
      const subtitlePaths = paths.filter((p) => {
        const name = fileNameFromPath(p);
        return isNativeAss(name) || isConvertible(name);
      });

      if (subtitlePaths.length === 0) {
        const msg = t("msg_no_subtitle_in_drop");
        addLog(msg, "error");
        setDropError(msg);
        return;
      }

      const conflictMsg = buildConflictMessage(subtitlePaths, "hdr", isFileInUse, t);
      if (conflictMsg) {
        setDropError(conflictMsg);
        return;
      }
      setDropError(null);

      const names = subtitlePaths.map(fileNameFromPath);
      setHdrFiles({ filePaths: subtitlePaths, fileNames: names });
    },
    [isFileInUse, setHdrFiles, setDropError, addLog, t]
  );

  useFolderDrop({
    ref: dropZoneRef,
    onPaths: handleDroppedPaths,
    onActiveChange: setDropActive,
    onError: (e) => setDropError(sanitizeError(e)),
    disabled: processing,
    t,
  });

  const handleConvert = useCallback(async () => {
    if (
      isHdrConvertDisabled({
        hasFiles: !!hdrFiles,
        processing,
        brightnessInvalid,
        templateInvalid,
        fpsInvalid,
      })
    ) {
      return;
    }
    if (!hdrFiles) return;
    if (busyRef.current) return;
    busyRef.current = true;
    pickGenRef.current += 1;
    const controller = new AbortController();
    abortRef.current = controller;
    setProcessing(true);
    try {
      if (brightness < MIN_BRIGHTNESS || brightness > MAX_BRIGHTNESS) {
        addLog(t("msg_invalid_brightness", MIN_BRIGHTNESS, MAX_BRIGHTNESS), "error");
        return;
      }

      const paths = hdrFiles.filePaths;
      const selectedInputKeys = new Set(paths.map(normalizeOutputKey));

      const projectedOutputs: string[] = [];
      for (const filePath of paths) {
        try {
          const outputPath = resolveOutputPath(filePath, activeTemplate, eotf);
          assertOutputDoesNotReplaceSelectedInput(outputPath, selectedInputKeys);
          projectedOutputs.push(outputPath);
        } catch {
          // Report this target with its input context in the processing loop.
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
          addLog(t("msg_cancelled"), "info");
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
            addLog(t("msg_cancelled"), "info");
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
        addLog(t("msg_start_conversion", paths.length, eotf, brightness));

        const outputPaths = new Set<string>();
        let successCount = 0;
        let warnedCount = 0;
        let processedCount = 0;

        for (const filePath of paths) {
          if (abortRef.current?.signal.aborted) {
            addLog(t("msg_cancelled"), "info");
            break;
          }

          let fileName = sanitizeForDialog(filePath.split(/[\\/]/).pop() ?? filePath);
          try {
            fileName = sanitizeForDialog(fileNameFromPath(filePath));
          } catch {
            // Keep the basename — better than no attribution.
          }
          addLog(t("msg_processing", fileName));

          try {
            if (!isNativeAss(fileName) && !isConvertible(fileName)) {
              addLog(t("msg_unsupported", fileName), "error");
              continue;
            }

            let outputPath: string;
            try {
              outputPath = resolveOutputPath(filePath, activeTemplate, eotf);
              assertOutputDoesNotReplaceSelectedInput(outputPath, selectedInputKeys);
              assertOutputDoesNotReplaceSelectedInput(outputPath, conflictingOutputs);
            } catch (e) {
              addLog(t("msg_skipped", fileName, sanitizeError(e)), "error");
              continue;
            }

            const normalizedOut = normalizeOutputKey(outputPath);
            if (outputPaths.has(normalizedOut)) {
              addLog(t("msg_skipped_duplicate", fileName), "error");
              continue;
            }
            outputPaths.add(normalizedOut);

            let content: string;
            let fileWarned = false;
            try {
              content = await readText(filePath, (read) => {
                const warnings = textDecodingWarnings(read, fileName, t);
                fileWarned = warnings.length > 0;
                for (const warning of warnings) addLog(warning, "warn");
              });
            } catch (e) {
              addLog(t("msg_read_error", fileName, sanitizeError(e)), "error");
              continue;
            }

            if (abortRef.current?.signal.aborted) break;

            let assContent: string;

            if (isNativeAss(fileName)) {
              assContent = processAssContent(content, brightness, eotf);
            } else if (isConvertible(fileName)) {
              const fpsOverride = parseHdrFpsOverrideForInput(fileName, fpsMode, fpsText);
              if (fpsOverride === null) throw new Error(t("style_fps_invalid"));

              const { content: rawAss, skippedCount } = convertTextCueSubtitleToAss(
                content,
                style,
                fpsOverride
              );
              if (skippedCount > 0) {
                fileWarned = true;
                addLog(t("msg_oversized_skipped", skippedCount, fileName), "warn");
              }

              assContent = processAssContent(rawAss, brightness, eotf);
            } else {
              continue;
            }

            if (abortRef.current?.signal.aborted) break;

            await writeText(outputPath, assContent, existingOutputs.has(normalizedOut));
            const outName = sanitizeForDialog(fileNameFromPath(outputPath));
            addLog(t("msg_done", outName), fileWarned ? "warn" : "success");
            if (fileWarned) warnedCount++;
            successCount++;
          } catch (e) {
            addLog(t("msg_convert_error", fileName, sanitizeError(e)), "error");
          } finally {
            processedCount++;
            setProgress({ processed: processedCount, total: paths.length });
          }
        }

        const aborted = !!abortRef.current?.signal.aborted;
        if (aborted) {
          setLastActionResult("cancelled");
        } else if (successCount > 0) {
          if (warnedCount > 0) {
            addLog(t("msg_complete_warnings", successCount, paths.length, warnedCount), "warn");
            setLastActionResult("partial");
          } else {
            addLog(t("msg_complete", successCount, paths.length), "success");
            setLastActionResult("success");
          }
        } else {
          addLog(t("msg_all_failed", paths.length), "error");
          setLastActionResult("error");
        }
      } finally {
        setProgress(null);
      }
    } finally {
      setProcessing(false);
      busyRef.current = false;
    }
  }, [
    hdrFiles,
    findLoadedInputConflictKeys,
    processing,
    brightnessInvalid,
    templateInvalid,
    fpsInvalid,
    brightness,
    eotf,
    activeTemplate,
    style,
    fpsMode,
    fpsText,
    addLog,
    t,
  ]);

  const handleClearFiles = useCallback(() => {
    clearFile("hdr");
    setDropError(null);
  }, [clearFile]);

  const requestCancel = useCallback(() => {
    abortRef.current?.abort();
  }, []);

  return {
    t,
    hdrFiles,
    eotf,
    setEotf,
    brightness,
    brightnessText,
    brightnessInvalid,
    handleBrightnessChange,
    handleBrightnessFromNits,
    template,
    setTemplate,
    customTemplate,
    setCustomTemplate,
    templateInvalid,
    showStylePanel,
    setShowStylePanel,
    style,
    setStyle,
    fpsMode,
    setFpsMode,
    fpsText,
    setFpsText,
    fpsInputInvalid,
    fpsInvalid,
    processing,
    convertDisabled,
    showFileList,
    setShowFileList,
    dropActive,
    dropError,
    setDropError,
    fileContainerRef,
    dropZoneRef,
    handleSelectFiles,
    handleClearFiles,
    handleConvert,
    requestCancel,
    logs,
    clearLogs,
    logScrollRef,
  };
}

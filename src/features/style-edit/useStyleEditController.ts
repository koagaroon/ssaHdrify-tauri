import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { useI18n } from "../../i18n/useI18n";
import {
  buildConflictMessage,
  normalizeOutputKey,
  sanitizeError,
  sanitizeForDialog,
} from "../../lib/dedup-helpers";
import { useFileContext } from "../../lib/FileContext";
import type { Status } from "../../lib/StatusContext";
import {
  fileNameFromPath,
  isInferredUtf16,
  outputPathExists,
  pickAssFiles,
  readTextDetectEncoding,
  writeStyleEditOutput,
} from "../../lib/tauri-api";
import { useClickOutside } from "../../lib/useClickOutside";
import { useFolderDrop } from "../../lib/useFolderDrop";
import { useLogPanel } from "../../lib/useLogPanel";
import { useTabStatus } from "../../lib/useTabStatus";
import {
  createStyleDocumentPlanner,
  type StyleEditOperations,
  type StyleEditRow,
} from "./style-editor";
import { deriveStyledPath } from "./style-output";
import {
  filterAndDedupeStyleEditPaths,
  isStyleEditWriteDisabled,
  reconcileStyleSelection,
  STYLE_EDIT_MAX_DECODED_BYTES,
  STYLE_EDIT_MAX_FILES,
  STYLE_EDIT_MAX_ROWS,
  STYLE_EDIT_MAX_SOURCE_BYTES,
  validateStyleEditOperations,
  type FontFamilyValidationError,
} from "./style-ui-state";

const ASS_EXTENSIONS = new Set(["ass", "ssa"]);

interface LoadedStyleFile {
  path: string;
  name: string;
  sourceRevision: string;
  inferredEncodingId?: string;
  planner: ReturnType<typeof createStyleDocumentPlanner>;
}

export interface StylePreviewRow extends StyleEditRow {
  key: string;
  filePath: string;
  fileName: string;
}

export interface StylePreviewError {
  kind: "document" | "output";
  reason: string;
}

type LastActionResult = "success" | "partial" | "error" | "cancelled" | "noop" | null;

function hasAssExtension(path: string): boolean {
  const name = fileNameFromPath(path);
  const dot = name.lastIndexOf(".");
  return dot >= 0 && ASS_EXTENSIONS.has(name.slice(dot + 1).toLowerCase());
}

export function familyErrorKey(error: FontFamilyValidationError): string {
  return `style_font_error_${error}`;
}

export function safeFileName(path: string): string {
  try {
    return sanitizeForDialog(fileNameFromPath(path));
  } catch {
    return sanitizeForDialog(path.split(/[\\/]/).pop() ?? path);
  }
}

export function useStyleEditController() {
  const { t } = useI18n();
  const { styleFiles, setStyleFiles, clearFile, isFileInUse } = useFileContext();
  const { logs, addLog, clearLogs, logScrollRef } = useLogPanel();

  const [loadedFiles, setLoadedFiles] = useState<LoadedStyleFile[]>([]);
  const [fontFamilyEnabled, setFontFamilyEnabled] = useState(false);
  const [targetFontFamily, setTargetFontFamily] = useState("");
  const [sourceFilterEnabled, setSourceFilterEnabled] = useState(false);
  const [sourceFontFamily, setSourceFontFamily] = useState("");
  const [fontSizeEnabled, setFontSizeEnabled] = useState(false);
  const [targetFontSize, setTargetFontSize] = useState("48");
  const [selectedRows, setSelectedRows] = useState<Set<string>>(new Set());
  const [selectionReady, setSelectionReady] = useState(false);
  const [analyzing, setAnalyzing] = useState(false);
  const [writing, setWriting] = useState(false);
  const [progress, setProgress] = useState<{ processed: number; total: number } | null>(null);
  const [lastActionResult, setLastActionResult] = useState<LastActionResult>(null);
  const [dropActive, setDropActive] = useState(false);
  const [dropError, setDropError] = useState<string | null>(null);
  const [showFileList, setShowFileList] = useState(false);

  const pickGenerationRef = useRef(0);
  const busyRef = useRef(false);
  const previousChangeableKeysRef = useRef<Set<string>>(new Set());
  const abortRef = useRef<AbortController | null>(null);
  const dropZoneRef = useRef<HTMLDivElement>(null);
  const fileContainerRef = useRef<HTMLDivElement>(null);

  const filePaths = useMemo(() => styleFiles?.filePaths ?? [], [styleFiles]);
  const fileNames = useMemo(() => styleFiles?.fileNames ?? [], [styleFiles]);
  const fileCount = filePaths.length;

  const validation = useMemo(
    () =>
      validateStyleEditOperations({
        fontFamilyEnabled,
        targetFontFamily,
        sourceFilterEnabled,
        sourceFontFamily,
        fontSizeEnabled,
        targetFontSize,
      }),
    [
      fontFamilyEnabled,
      targetFontFamily,
      sourceFilterEnabled,
      sourceFontFamily,
      fontSizeEnabled,
      targetFontSize,
    ]
  );

  const operations = useMemo<StyleEditOperations>(() => {
    if (!validation.valid) return {};
    return {
      ...(fontFamilyEnabled
        ? {
            fontFamily: {
              enabled: true,
              targetFamily: validation.targetFontFamily!,
              ...(sourceFilterEnabled ? { sourceFamily: validation.sourceFontFamily! } : {}),
            },
          }
        : {}),
      ...(fontSizeEnabled
        ? { fontSize: { enabled: true, targetSize: validation.targetFontSize! } }
        : {}),
    };
  }, [validation, fontFamilyEnabled, sourceFilterEnabled, fontSizeEnabled]);

  const preview = useMemo(() => {
    const rows: StylePreviewRow[] = [];
    const errors = new Map<string, StylePreviewError>();
    const outputPaths = new Map<string, string>();
    if (!validation.valid) return { rows, errors, outputPaths };
    for (const file of loadedFiles) {
      let outputPath: string;
      try {
        outputPath = deriveStyledPath(file.path);
      } catch (error) {
        errors.set(file.path, { kind: "output", reason: sanitizeError(error) });
        continue;
      }
      try {
        const plan = file.planner.plan(operations);
        for (const row of plan.rows) {
          rows.push({
            ...row,
            key: `${file.path}\u001f${row.id}`,
            filePath: file.path,
            fileName: file.name,
          });
        }
        outputPaths.set(file.path, outputPath);
      } catch (error) {
        errors.set(file.path, { kind: "document", reason: sanitizeError(error) });
      }
    }
    return { rows, errors, outputPaths };
  }, [loadedFiles, operations, validation.valid]);

  const changeableKeys = useMemo(
    () => new Set(preview.rows.filter((row) => row.willChange).map((row) => row.key)),
    [preview.rows]
  );

  useEffect(() => {
    if (!validation.valid) return;
    const previous = previousChangeableKeysRef.current;
    setSelectedRows((current) => reconcileStyleSelection(current, previous, changeableKeys));
    previousChangeableKeysRef.current = new Set(changeableKeys);
    setSelectionReady(true);
    setLastActionResult(null);
  }, [changeableKeys, validation.valid]);

  const effectiveSelectedRows = useMemo(
    () => preview.rows.filter((row) => row.willChange && selectedRows.has(row.key)),
    [preview.rows, selectedRows]
  );
  const effectiveSelectedCount = effectiveSelectedRows.length;

  const previewEmptyMessage = useMemo(() => {
    if (fileCount === 0) return t("style_no_preview");
    if (!validation.hasEnabledOperation) return t("status_style_needs_operation");
    if (!validation.valid) return t("status_style_invalid_operation");
    if (preview.errors.size > 0) return t("style_row_parse_error");
    return t("style_no_editable_styles");
  }, [fileCount, preview.errors.size, validation.hasEnabledOperation, validation.valid, t]);

  const writeDisabled = isStyleEditWriteDisabled({
    fileCount,
    busy: analyzing || writing,
    operationsValid: validation.valid && preview.errors.size === 0,
    effectiveSelectedRowCount: selectionReady ? effectiveSelectedCount : 0,
  });

  useClickOutside(showFileList, fileContainerRef, () => setShowFileList(false));

  const ingestPaths = useCallback(
    async (paths: string[], generation: number) => {
      setLastActionResult(null);
      const uniquePaths = filterAndDedupeStyleEditPaths(paths);
      if (uniquePaths.length === 0) {
        setDropError(t("msg_no_ass_in_drop"));
        return;
      }
      const conflictMessage = buildConflictMessage(uniquePaths, "style", isFileInUse, t);
      if (conflictMessage) {
        setDropError(conflictMessage);
        return;
      }
      if (uniquePaths.length > STYLE_EDIT_MAX_FILES) {
        setDropError(t("err_batch_too_many_files", uniquePaths.length, STYLE_EDIT_MAX_FILES));
        return;
      }

      setDropError(null);
      setAnalyzing(true);
      const nextFiles: LoadedStyleFile[] = [];
      let aggregateSourceBytes = 0;
      let aggregateDecodedBytes = 0;
      let aggregateRows = 0;
      try {
        for (const path of uniquePaths) {
          if (generation !== pickGenerationRef.current) return;
          const name = safeFileName(path);
          const read = await readTextDetectEncoding(path);
          if (generation !== pickGenerationRef.current) return;
          if (read.lossy) {
            throw new Error(t("msg_style_lossy_encoding", name));
          }
          if (isInferredUtf16(read)) {
            addLog(t("msg_inferred_utf16", name, read.encodingId), "warn");
          }
          aggregateSourceBytes += read.sourceByteLength;
          if (aggregateSourceBytes > STYLE_EDIT_MAX_SOURCE_BYTES) {
            const reachedMb = Math.ceil(aggregateSourceBytes / (1024 * 1024));
            const capMb = Math.round(STYLE_EDIT_MAX_SOURCE_BYTES / (1024 * 1024));
            throw new Error(t("msg_style_source_bytes_too_large", reachedMb, capMb));
          }
          aggregateDecodedBytes += read.text.length * 2;
          if (aggregateDecodedBytes > STYLE_EDIT_MAX_DECODED_BYTES) {
            const reachedMb = Math.ceil(aggregateDecodedBytes / (1024 * 1024));
            const capMb = Math.round(STYLE_EDIT_MAX_DECODED_BYTES / (1024 * 1024));
            throw new Error(t("msg_style_decoded_bytes_too_large", reachedMb, capMb));
          }
          const planner = createStyleDocumentPlanner(read.text);
          aggregateRows += planner.inspect.styleCount;
          if (aggregateRows > STYLE_EDIT_MAX_ROWS) {
            throw new Error(t("msg_style_too_many_rows", STYLE_EDIT_MAX_ROWS));
          }
          nextFiles.push({
            path,
            name,
            sourceRevision: read.sourceRevision,
            ...(isInferredUtf16(read) && { inferredEncodingId: read.encodingId }),
            planner,
          });
        }

        if (generation !== pickGenerationRef.current) return;
        setSelectionReady(false);
        previousChangeableKeysRef.current = new Set();
        setSelectedRows(new Set());
        setLoadedFiles(nextFiles);
        setStyleFiles({
          filePaths: nextFiles.map((file) => file.path),
          fileNames: nextFiles.map((file) => file.name),
        });
      } catch (error) {
        if (generation !== pickGenerationRef.current) return;
        const message = sanitizeError(error);
        setDropError(message);
        addLog(t("error_prefix", message), "error");
      } finally {
        if (generation === pickGenerationRef.current) setAnalyzing(false);
      }
    },
    [isFileInUse, setStyleFiles, addLog, t]
  );

  const handlePickFiles = useCallback(async () => {
    const generation = (pickGenerationRef.current += 1);
    setLastActionResult(null);
    try {
      const paths = await pickAssFiles(t);
      if (generation !== pickGenerationRef.current || !paths?.length) return;
      await ingestPaths(paths, generation);
    } catch (error) {
      if (generation !== pickGenerationRef.current) return;
      addLog(t("error_prefix", sanitizeError(error)), "error");
    }
  }, [ingestPaths, addLog, t]);

  const handleDroppedPaths = useCallback(
    async (paths: string[]) => {
      const generation = (pickGenerationRef.current += 1);
      setLastActionResult(null);
      const assPaths = paths.filter(hasAssExtension);
      if (assPaths.length === 0) {
        const message = t("msg_no_ass_in_drop");
        setDropError(message);
        addLog(message, "error");
        return;
      }
      await ingestPaths(assPaths, generation);
    },
    [ingestPaths, addLog, t]
  );

  useFolderDrop({
    ref: dropZoneRef,
    onPaths: (paths) => void handleDroppedPaths(paths),
    onActiveChange: setDropActive,
    onError: (error) => {
      const message = sanitizeError(error);
      setDropError(message);
      addLog(t("error_prefix", message), "error");
    },
    disabled: analyzing || writing,
    t,
  });

  const handleClearFiles = useCallback(() => {
    pickGenerationRef.current += 1;
    abortRef.current?.abort();
    clearFile("style");
    setLoadedFiles([]);
    setSelectedRows(new Set());
    setSelectionReady(false);
    previousChangeableKeysRef.current = new Set();
    setAnalyzing(false);
    setWriting(false);
    setProgress(null);
    setDropError(null);
    setLastActionResult(null);
  }, [clearFile]);

  const handleWrite = useCallback(async () => {
    if (writeDisabled || busyRef.current) return;
    busyRef.current = true;
    const abort = new AbortController();
    abortRef.current = abort;
    setWriting(true);
    setProgress(null);
    setDropError(null);
    setLastActionResult(null);

    try {
      if (preview.errors.size > 0) {
        for (const [path, error] of preview.errors) {
          const key =
            error.kind === "output" ? "msg_style_output_path_error" : "msg_style_parse_error";
          addLog(t(key, safeFileName(path), error.reason), "error");
        }
        setLastActionResult("error");
        return;
      }

      const writableTargets = loadedFiles.flatMap((file) => {
        const selectedRowIds = preview.rows
          .filter((row) => row.filePath === file.path && selectedRows.has(row.key))
          .map((row) => row.id);
        const outputPath = preview.outputPaths.get(file.path);
        return selectedRowIds.length > 0 && outputPath
          ? [{ file, outputPath, selectedRowIds }]
          : [];
      });

      if (writableTargets.length === 0) {
        addLog(t("msg_style_all_noop"), "info");
        setLastActionResult("noop");
        return;
      }

      const outputKeys = writableTargets.map((target) => normalizeOutputKey(target.outputPath));
      if (new Set(outputKeys).size !== outputKeys.length) {
        const message = t("msg_style_duplicate_outputs");
        setDropError(message);
        addLog(message, "error");
        setLastActionResult("error");
        return;
      }

      let existingCount = 0;
      for (const target of writableTargets) {
        if (abort.signal.aborted) {
          addLog(t("msg_style_cancelled"), "warn");
          setLastActionResult("cancelled");
          return;
        }
        try {
          if (await outputPathExists(target.outputPath)) existingCount += 1;
        } catch (error) {
          const message = t(
            "msg_style_output_probe_error",
            safeFileName(target.outputPath),
            sanitizeError(error)
          );
          setDropError(message);
          addLog(message, "error");
          setLastActionResult("error");
          return;
        }
      }
      if (existingCount > 0) {
        const message = t("msg_style_outputs_exist", existingCount);
        setDropError(message);
        addLog(message, "error");
        setLastActionResult("error");
        return;
      }

      setProgress({ processed: 0, total: writableTargets.length });
      addLog(t("msg_style_start", writableTargets.length, effectiveSelectedCount), "info");
      let written = 0;
      let failed = 0;
      for (let index = 0; index < writableTargets.length; index += 1) {
        if (abort.signal.aborted) break;
        const target = writableTargets[index]!;
        try {
          const result = target.file.planner.apply(operations, target.selectedRowIds);
          if (target.file.inferredEncodingId) {
            addLog(
              t("msg_inferred_utf16", target.file.name, target.file.inferredEncodingId),
              "warn"
            );
          }
          await writeStyleEditOutput({
            sourcePath: target.file.path,
            expectedRevision: target.file.sourceRevision,
            outputPath: target.outputPath,
            content: result.content,
          });
          written += 1;
          addLog(
            t("msg_style_written", safeFileName(target.outputPath), result.changedStyleCount),
            "success"
          );
        } catch (error) {
          failed += 1;
          addLog(t("msg_style_write_error", target.file.name, sanitizeError(error)), "error");
        }
        setProgress({ processed: index + 1, total: writableTargets.length });
      }

      const processed = written + failed;
      if (abort.signal.aborted && processed < writableTargets.length) {
        addLog(
          t("msg_style_cancelled_summary", written, failed, writableTargets.length - processed),
          "warn"
        );
        setLastActionResult("cancelled");
      } else {
        const unchanged = loadedFiles.length - writableTargets.length;
        addLog(t("msg_style_complete", written, unchanged, failed), failed ? "warn" : "success");
        if (written === 0 && failed > 0) setLastActionResult("error");
        else if (failed > 0) setLastActionResult("partial");
        else {
          setDropError(null);
          setLastActionResult("success");
        }
      }
    } catch (error) {
      addLog(t("error_prefix", sanitizeError(error)), "error");
      setLastActionResult("error");
    } finally {
      abortRef.current = null;
      setWriting(false);
      setProgress(null);
      busyRef.current = false;
    }
  }, [
    writeDisabled,
    preview,
    loadedFiles,
    selectedRows,
    operations,
    effectiveSelectedCount,
    addLog,
    t,
  ]);

  const toggleRowSelection = useCallback((rowKey: string) => {
    setSelectedRows((current) => {
      const next = new Set(current);
      if (next.has(rowKey)) next.delete(rowKey);
      else next.add(rowKey);
      return next;
    });
    setLastActionResult(null);
  }, []);

  const selectAllRows = useCallback(() => {
    setSelectedRows(new Set(changeableKeys));
    setLastActionResult(null);
  }, [changeableKeys]);

  const clearRowSelection = useCallback(() => {
    setSelectedRows(new Set());
    setLastActionResult(null);
  }, []);

  const requestCancel = useCallback(() => {
    abortRef.current?.abort();
  }, []);

  const tabStatus = useMemo<Status>(() => {
    if (fileCount === 0) return { kind: "idle", message: t("status_style_idle") };
    if (analyzing) return { kind: "busy", message: t("status_style_analyzing") };
    if (writing) {
      return { kind: "busy", message: t("status_style_busy"), progress: progress ?? undefined };
    }
    if (!validation.hasEnabledOperation) {
      return { kind: "pending", message: t("status_style_needs_operation") };
    }
    if (!validation.valid) {
      return { kind: "error", message: t("status_style_invalid_operation") };
    }
    if (preview.errors.size > 0) {
      return { kind: "error", message: t("status_style_preview_error") };
    }
    if (lastActionResult === "success") return { kind: "done", message: t("status_style_done") };
    if (lastActionResult === "partial")
      return { kind: "pending", message: t("status_style_partial") };
    if (lastActionResult === "error") return { kind: "error", message: t("status_style_error") };
    if (lastActionResult === "cancelled") {
      return { kind: "pending", message: t("status_style_cancelled") };
    }
    if (lastActionResult === "noop") return { kind: "pending", message: t("status_style_noop") };
    if (effectiveSelectedCount === 0) {
      return { kind: "pending", message: t("status_style_noop") };
    }
    return {
      kind: "pending",
      message: t("status_style_pending", effectiveSelectedCount, fileCount),
    };
  }, [
    fileCount,
    analyzing,
    writing,
    progress,
    lastActionResult,
    validation.hasEnabledOperation,
    validation.valid,
    preview.errors.size,
    effectiveSelectedCount,
    t,
  ]);
  useTabStatus("style", tabStatus);

  return {
    t,
    filePaths,
    fileNames,
    fileCount,
    fontFamilyEnabled,
    setFontFamilyEnabled,
    targetFontFamily,
    setTargetFontFamily,
    sourceFilterEnabled,
    setSourceFilterEnabled,
    sourceFontFamily,
    setSourceFontFamily,
    fontSizeEnabled,
    setFontSizeEnabled,
    targetFontSize,
    setTargetFontSize,
    validation,
    preview,
    changeableKeys,
    selectedRows,
    toggleRowSelection,
    selectAllRows,
    clearRowSelection,
    effectiveSelectedCount,
    previewEmptyMessage,
    analyzing,
    writing,
    writeDisabled,
    dropActive,
    dropError,
    setDropError,
    showFileList,
    setShowFileList,
    dropZoneRef,
    fileContainerRef,
    handlePickFiles,
    handleClearFiles,
    handleWrite,
    requestCancel,
    logs,
    clearLogs,
    logScrollRef,
  };
}

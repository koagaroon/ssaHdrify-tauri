import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { useI18n } from "../../i18n/useI18n";
import {
  buildConflictMessage,
  normalizeOutputKey,
  sanitizeError,
  sanitizeForDialog,
} from "../../lib/dedup-helpers";
import { DropErrorBanner } from "../../lib/DropErrorBanner";
import { useFileContext } from "../../lib/FileContext";
import { LogPanel } from "../../lib/LogPanel";
import NumberInput from "../../lib/NumberInput";
import { PreviewTable, type PreviewTableColumn } from "../../lib/PreviewTable";
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
  STYLE_EDIT_MAX_FONT_SIZE,
  STYLE_EDIT_MAX_ROWS,
  STYLE_EDIT_MAX_SOURCE_BYTES,
  STYLE_EDIT_MIN_FONT_SIZE,
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

interface PreviewRow extends StyleEditRow {
  key: string;
  filePath: string;
  fileName: string;
}

interface PreviewError {
  kind: "document" | "output";
  reason: string;
}

type LastActionResult = "success" | "partial" | "error" | "cancelled" | "noop" | null;

function hasAssExtension(path: string): boolean {
  const name = fileNameFromPath(path);
  const dot = name.lastIndexOf(".");
  return dot >= 0 && ASS_EXTENSIONS.has(name.slice(dot + 1).toLowerCase());
}

function familyErrorKey(error: FontFamilyValidationError): string {
  return `style_font_error_${error}`;
}

function safeFileName(path: string): string {
  try {
    return sanitizeForDialog(fileNameFromPath(path));
  } catch {
    return sanitizeForDialog(path.split(/[\\/]/).pop() ?? path);
  }
}

function valueChange(before: string | null, after: string | null, changes: boolean) {
  const visibleBefore = before || "—";
  const visibleAfter = after || "—";
  if (!changes) return <span title={visibleBefore}>{visibleBefore}</span>;
  return (
    <span className="style-preview-value" title={`${visibleBefore} → ${visibleAfter}`}>
      <span className="style-preview-before">{visibleBefore}</span>
      <span className="style-preview-arrow" aria-hidden="true">
        →
      </span>
      <span className="style-preview-after">{visibleAfter}</span>
    </span>
  );
}

export default function StyleEdit() {
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
    const rows: PreviewRow[] = [];
    const errors = new Map<string, PreviewError>();
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
    // Select genuinely new rows by default, while retaining manual
    // deselection for rows that remain eligible as options change.
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

  const previewColumns = useMemo<PreviewTableColumn<PreviewRow>[]>(
    () => [
      {
        key: "select",
        header: "",
        width: "28px",
        render: (row) => (
          <input
            type="checkbox"
            checked={selectedRows.has(row.key)}
            disabled={!row.willChange || analyzing || writing}
            aria-label={t("style_row_select_aria", row.styleName, row.fileName)}
            onChange={() => {
              setSelectedRows((current) => {
                const next = new Set(current);
                if (next.has(row.key)) next.delete(row.key);
                else next.add(row.key);
                return next;
              });
              setLastActionResult(null);
            }}
          />
        ),
      },
      {
        key: "file",
        header: t("style_col_file"),
        width: "minmax(100px, 0.9fr)",
        render: (row) => <span title={row.fileName}>{row.fileName}</span>,
      },
      {
        key: "style",
        header: t("style_col_style"),
        width: "minmax(80px, 0.7fr)",
        render: (row) => <span title={row.styleName}>{row.styleName}</span>,
      },
      {
        key: "font",
        header: t("style_col_font_family"),
        width: "minmax(130px, 1.2fr)",
        render: (row) =>
          valueChange(
            row.fontFamilyBefore,
            row.fontFamilyAfter,
            row.changes.includes("fontFamily")
          ),
      },
      {
        key: "size",
        header: t("style_col_font_size"),
        width: "minmax(70px, 0.6fr)",
        render: (row) =>
          valueChange(row.fontSizeBefore, row.fontSizeAfter, row.changes.includes("fontSize")),
      },
      {
        key: "result",
        header: t("style_col_result"),
        width: "90px",
        render: (row) => (
          <span
            className={row.willChange ? "style-preview-result-change" : "style-preview-result-noop"}
          >
            {t(row.willChange ? "style_row_will_change" : "style_row_no_change")}
          </span>
        ),
      },
    ],
    [selectedRows, analyzing, writing, t]
  );

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

  return (
    <div className="space-y-4">
      <div
        ref={dropZoneRef}
        className={`drop-zone flex items-center gap-2${dropActive ? " drop-active" : ""}`}
      >
        <div ref={fileContainerRef} className="flex-1 min-w-0" style={{ position: "relative" }}>
          {fileCount > 1 ? (
            <button
              type="button"
              onClick={() => setShowFileList((open) => !open)}
              className="w-full flex items-center gap-2 px-3 rounded-lg text-sm"
              style={{
                background: "var(--bg-panel)",
                border: "1px solid var(--border-light)",
                minHeight: "38px",
                color: "var(--text-primary)",
                textAlign: "left",
              }}
              aria-expanded={showFileList}
              aria-haspopup="listbox"
            >
              <span className="truncate flex-1">{fileNames.join(", ")}</span>
              <span className="flex-none text-xs" style={{ color: "var(--text-muted)" }}>
                ({fileCount}) {showFileList ? "▲" : "▼"}
              </span>
            </button>
          ) : (
            <div
              className="flex items-center gap-2 px-3 rounded-lg text-sm"
              style={{
                background: fileCount ? "var(--bg-panel)" : "var(--bg-input)",
                border: "1px solid var(--border-light)",
                minHeight: "38px",
              }}
            >
              <span
                className={`${fileCount ? "truncate" : "italic"} flex-1`}
                style={{ color: fileCount ? "var(--text-primary)" : "var(--text-muted)" }}
              >
                {fileNames[0] ?? t("file_empty")}
              </span>
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
              <div className="px-3 py-2" style={{ borderBottom: "1px solid var(--border)" }}>
                <span className="text-xs font-medium" style={{ color: "var(--text-muted)" }}>
                  {t("files_selected_title", fileCount)}
                </span>
              </div>
              <div className="overflow-y-auto">
                {fileNames.map((name, index) => (
                  <div
                    key={filePaths[index] ?? name}
                    className="px-3 py-2 text-sm truncate"
                    style={{ color: "var(--text-primary)" }}
                    title={filePaths[index]}
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
            type="button"
            onClick={handleClearFiles}
            disabled={analyzing || writing}
            className="flex-none px-3 rounded-lg text-lg font-bold transition-colors"
            style={{
              background: analyzing || writing ? "var(--bg-input)" : "var(--cancel-bg)",
              color: analyzing || writing ? "var(--text-muted)" : "var(--cancel-text)",
              height: "38px",
            }}
            title={t("btn_clear_file")}
          >
            ✕
          </button>
        )}
        <button
          type="button"
          onClick={handlePickFiles}
          disabled={analyzing || writing}
          className="flex-none px-5 rounded-lg font-medium text-sm transition-colors"
          style={{
            background: analyzing || writing ? "var(--bg-input)" : "var(--accent)",
            color: analyzing || writing ? "var(--text-muted)" : "white",
            height: "38px",
          }}
        >
          {analyzing ? t("btn_analyzing") : t("btn_select_files")}
        </button>
        {writing && (
          <button
            type="button"
            onClick={() => abortRef.current?.abort()}
            className="flex-none px-4 rounded-lg text-sm"
            style={{ background: "var(--cancel-bg)", color: "var(--cancel-text)", height: "38px" }}
          >
            {t("btn_cancel")}
          </button>
        )}
        <button
          type="button"
          onClick={() => void handleWrite()}
          disabled={writeDisabled}
          className="flex-none px-6 rounded-lg font-medium text-sm transition-colors"
          style={{
            background: writeDisabled ? "var(--accent-disabled-bg)" : "var(--accent)",
            color: writeDisabled ? "var(--accent-disabled-text)" : "white",
            height: "38px",
            minWidth: "120px",
          }}
        >
          {writing
            ? t("btn_writing_style_edits")
            : t("btn_write_style_edits", effectiveSelectedCount)}
        </button>
      </div>

      <DropErrorBanner message={dropError} onDismiss={() => setDropError(null)} />
      {fileCount === 0 && !dropError && (
        <p className="text-xs ml-1" style={{ color: "var(--text-muted)" }}>
          {t("style_drop_hint")}
        </p>
      )}

      <section className="style-operations-panel" aria-labelledby="style-operations-heading">
        <div className="style-operations-heading">
          <strong id="style-operations-heading">{t("style_operations_title")}</strong>
          <span>{t("style_operations_hint")}</span>
        </div>
        <div className="style-operation-grid">
          <div className="style-operation" data-enabled={fontFamilyEnabled}>
            <label className="style-operation-toggle">
              <input
                type="checkbox"
                checked={fontFamilyEnabled}
                disabled={analyzing || writing}
                onChange={(event) => setFontFamilyEnabled(event.target.checked)}
              />
              {t("style_change_font_family")}
            </label>
            <div className="style-operation-fields">
              <label className="style-field-label" htmlFor="style-target-family">
                {t("style_target_font_family")}
              </label>
              <input
                id="style-target-family"
                className="style-text-input"
                type="text"
                value={targetFontFamily}
                maxLength={128}
                disabled={!fontFamilyEnabled || analyzing || writing}
                aria-invalid={!!validation.targetFontError || undefined}
                aria-describedby={
                  validation.targetFontError ? "style-target-family-error" : undefined
                }
                placeholder={t("style_target_font_placeholder")}
                onChange={(event) => setTargetFontFamily(event.target.value)}
              />
              {validation.targetFontError && (
                <span id="style-target-family-error" className="style-field-error" role="alert">
                  {t(familyErrorKey(validation.targetFontError))}
                </span>
              )}
              <label className="style-source-filter-toggle">
                <input
                  type="checkbox"
                  checked={sourceFilterEnabled}
                  disabled={!fontFamilyEnabled || analyzing || writing}
                  onChange={(event) => setSourceFilterEnabled(event.target.checked)}
                />
                {t("style_filter_source_family")}
              </label>
              {sourceFilterEnabled && (
                <>
                  <label className="style-field-label" htmlFor="style-source-family">
                    {t("style_source_font_family")}
                  </label>
                  <input
                    id="style-source-family"
                    className="style-text-input"
                    type="text"
                    value={sourceFontFamily}
                    maxLength={128}
                    disabled={!fontFamilyEnabled || analyzing || writing}
                    aria-invalid={!!validation.sourceFontError || undefined}
                    aria-describedby={
                      validation.sourceFontError ? "style-source-family-error" : undefined
                    }
                    placeholder={t("style_source_font_placeholder")}
                    onChange={(event) => setSourceFontFamily(event.target.value)}
                  />
                  {validation.sourceFontError && (
                    <span id="style-source-family-error" className="style-field-error" role="alert">
                      {t(familyErrorKey(validation.sourceFontError))}
                    </span>
                  )}
                </>
              )}
            </div>
          </div>

          <div className="style-operation" data-enabled={fontSizeEnabled}>
            <label className="style-operation-toggle">
              <input
                type="checkbox"
                checked={fontSizeEnabled}
                disabled={analyzing || writing}
                onChange={(event) => setFontSizeEnabled(event.target.checked)}
              />
              {t("style_change_font_size")}
            </label>
            <div className="style-operation-fields">
              <label className="style-field-label" htmlFor="style-target-size">
                {t("style_target_font_size")} · {t("style_font_size_range")}
              </label>
              <NumberInput
                id="style-target-size"
                value={targetFontSize}
                onChange={setTargetFontSize}
                min={STYLE_EDIT_MIN_FONT_SIZE}
                max={STYLE_EDIT_MAX_FONT_SIZE}
                step="0.5"
                disabled={!fontSizeEnabled || analyzing || writing}
                invalid={validation.fontSizeInvalid}
                ariaDescribedBy={validation.fontSizeInvalid ? "style-target-size-error" : undefined}
              />
              {validation.fontSizeInvalid && (
                <span id="style-target-size-error" className="style-field-error" role="alert">
                  {t("style_font_size_error")}
                </span>
              )}
            </div>
          </div>
        </div>
        <div className="style-edit-note mt-3">{t("style_inline_untouched")}</div>
        <div className="style-edit-note">{t("style_output_note")}</div>
      </section>

      <PreviewTable
        rows={preview.rows}
        columns={previewColumns}
        rowKey={(row) => row.key}
        maxHeight="300px"
        className="style-preview-table"
        rowClassName={(row) => (row.willChange ? undefined : "style-preview-noop")}
        title={
          <div className="style-preview-title">
            <span className="style-preview-title-summary">
              {t("style_preview_title", preview.rows.length, fileCount)} ·{" "}
              {t("style_preview_change_summary", effectiveSelectedCount)}
            </span>
            <span className="style-preview-title-actions">
              <button
                type="button"
                className="style-preview-action"
                disabled={changeableKeys.size === 0 || analyzing || writing}
                onClick={() => {
                  setSelectedRows(new Set(changeableKeys));
                  setLastActionResult(null);
                }}
              >
                {t("style_select_all")}
              </button>
              <button
                type="button"
                className="style-preview-action"
                disabled={selectedRows.size === 0 || analyzing || writing}
                onClick={() => {
                  setSelectedRows(new Set());
                  setLastActionResult(null);
                }}
              >
                {t("style_clear_selection")}
              </button>
            </span>
          </div>
        }
        emptyMessage={previewEmptyMessage}
      />

      {preview.errors.size > 0 && (
        <div role="alert" className="space-y-1">
          {Array.from(preview.errors, ([path, error]) => (
            <p key={path} className="text-xs" style={{ color: "var(--error)" }}>
              {t(
                error.kind === "output" ? "msg_style_output_path_error" : "msg_style_parse_error",
                safeFileName(path),
                error.reason
              )}
            </p>
          ))}
        </div>
      )}

      <LogPanel logs={logs} onClear={clearLogs} scrollRef={logScrollRef} />
    </div>
  );
}

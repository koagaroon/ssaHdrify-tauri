import { useMemo } from "react";

import { DropErrorBanner } from "../../lib/DropErrorBanner";
import { LogPanel } from "../../lib/LogPanel";
import NumberInput from "../../lib/NumberInput";
import { PreviewTable, type PreviewTableColumn } from "../../lib/PreviewTable";
import { renderSubtitleFileStrip } from "../../lib/subtitle-file-strip";
import { STYLE_EDIT_MAX_FONT_SIZE, STYLE_EDIT_MIN_FONT_SIZE } from "./style-ui-state";
import {
  familyErrorKey,
  safeFileName,
  useStyleEditController,
  type StylePreviewRow,
} from "./useStyleEditController";

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
  const {
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
  } = useStyleEditController();

  const previewColumns = useMemo<PreviewTableColumn<StylePreviewRow>[]>(
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
            onChange={() => toggleRowSelection(row.key)}
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
    [selectedRows, analyzing, writing, toggleRowSelection, t]
  );

  return (
    <div className="space-y-4">
      {renderSubtitleFileStrip({
        dropZoneRef,
        fileContainerRef,
        dropActive,
        filePaths,
        fileNames,
        showFileList,
        onToggleFileList: () => setShowFileList((open) => !open),
        emptyLabel: t("file_empty"),
        selectedTitle: t("files_selected_title", fileCount),
        clearTitle: t("btn_clear_file"),
        selectLabel: analyzing ? t("btn_analyzing") : t("btn_select_files"),
        clearDisabled: analyzing || writing,
        selectDisabled: analyzing || writing,
        onClearFiles: handleClearFiles,
        onSelectFiles: handlePickFiles,
        trailingActions: (
          <>
            {writing && (
              <button
                type="button"
                onClick={requestCancel}
                className="flex-none px-4 rounded-lg text-sm"
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
              type="button"
              onClick={handleWrite}
              disabled={writeDisabled}
              className="flex-none px-6 rounded-lg font-medium text-sm transition-colors"
              style={{
                background: writeDisabled ? "var(--accent-disabled-bg)" : "var(--accent)",
                color: writeDisabled ? "var(--accent-disabled-text)" : "var(--accent-text)",
                height: "38px",
                minWidth: "120px",
              }}
            >
              {writing
                ? t("btn_writing_style_edits")
                : t("btn_write_style_edits", effectiveSelectedCount)}
            </button>
          </>
        ),
      })}

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
                onClick={selectAllRows}
              >
                {t("style_select_all")}
              </button>
              <button
                type="button"
                className="style-preview-action"
                disabled={selectedRows.size === 0 || analyzing || writing}
                onClick={clearRowSelection}
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

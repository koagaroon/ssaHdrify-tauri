import { List, type RowComponentProps } from "react-window";
import { LogPanel } from "../../lib/LogPanel";
import { DropErrorBanner } from "../../lib/DropErrorBanner";
import NumberInput from "../../lib/NumberInput";
import { sanitizeForDialog } from "../../lib/dedup-helpers";
import { formatDisplayTime } from "./timing-engine";
import { formatSignedOffsetMs } from "./timing-ui-state";
import {
  PREVIEW_ROW_HEIGHT,
  useTimingShiftController,
  type PreviewRowData,
  type Unit,
} from "./useTimingShiftController";

function PreviewRow({
  index,
  style,
  ariaAttributes,
  preview,
  formatTime,
  formatMatchedRule,
  origLabel,
  shiftedLabel,
}: RowComponentProps<PreviewRowData>) {
  const entry = preview[index]!;
  const matchedRuleInfo = formatMatchedRule(entry);
  const shiftedTitle = matchedRuleInfo
    ? `${shiftedLabel}: ${formatTime(entry.shiftedStart)} (${matchedRuleInfo})`
    : `${shiftedLabel}: ${formatTime(entry.shiftedStart)}`;
  return (
    <div
      {...ariaAttributes}
      style={style}
      className={"timeline-row" + (entry.wasShifted ? "" : " unchanged")}
    >
      <span className="idx">{entry.index}</span>
      <span className="t-orig" title={`${origLabel}: ${formatTime(entry.originalStart)}`}>
        {formatTime(entry.originalStart)}
      </span>
      <span className="t-new" title={shiftedTitle}>
        {formatTime(entry.shiftedStart)}
      </span>
      <span className="txt" title={entry.tooltipText}>
        {entry.text}
      </span>
    </div>
  );
}

export default function TimingShift() {
  const {
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
  } = useTimingShiftController();

  return (
    <div className="space-y-4">
      {/* File strip — drop zone + filename(s) + clear + Select + Cancel + Save */}
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
              {detectedFormat && (
                <span
                  className="flex-none px-2 py-0.5 rounded text-xs"
                  style={{
                    background: "var(--bg-input)",
                    color: "var(--text-muted)",
                  }}
                >
                  {detectedFormat}
                </span>
              )}
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
                <>
                  <span className="truncate flex-1" style={{ color: "var(--text-primary)" }}>
                    {primaryFileName}
                  </span>
                  {detectedFormat && (
                    <span
                      className="flex-none px-2 py-0.5 rounded text-xs"
                      style={{
                        background: "var(--bg-input)",
                        color: "var(--text-muted)",
                      }}
                    >
                      {detectedFormat}
                    </span>
                  )}
                  {captionCount > 0 && (
                    <span className="flex-none text-xs" style={{ color: "var(--text-muted)" }}>
                      {t("captions_count", captionCount)}
                    </span>
                  )}
                </>
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
            disabled={busy}
            className="flex-none px-3 rounded-lg text-lg font-bold transition-colors"
            style={{
              background: busy ? "var(--bg-input)" : "var(--cancel-bg)",
              color: busy ? "var(--text-muted)" : "var(--cancel-text)",
              height: "38px",
            }}
            title={t("btn_clear_file")}
          >
            ✕
          </button>
        )}
        <button
          onClick={handlePickFiles}
          disabled={busy}
          className="flex-none px-5 rounded-lg font-medium text-sm transition-colors"
          style={{
            background: busy ? "var(--bg-input)" : "var(--accent)",
            color: busy ? "var(--text-muted)" : "var(--accent-text)",
            height: "38px",
          }}
        >
          {t("btn_select_files")}
        </button>
        {busy && (
          <button
            onClick={requestCancel}
            className="flex-none px-4 rounded-lg text-sm transition-colors"
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
          onClick={handleSaveAll}
          disabled={saveDisabled}
          className="flex-none px-6 rounded-lg font-medium text-sm transition-colors"
          style={{
            background: saveDisabled ? "var(--bg-input)" : "var(--accent)",
            color: saveDisabled ? "var(--text-muted)" : "var(--accent-text)",
            height: "38px",
            minWidth: "120px",
          }}
        >
          {saveLabel}
        </button>
      </div>

      {/* Selection-rejected banner */}
      <DropErrorBanner message={dropError} onDismiss={() => setDropError(null)} />

      {/* Timing-map load/parse error banner */}
      <DropErrorBanner message={timingMapError} onDismiss={() => setTimingMapError(null)} />

      {/* Subtitle parse/cap error banner */}
      <DropErrorBanner message={previewError} onDismiss={dismissPreviewError} />

      {/* Drop-zone discoverability hint — visible only when idle. */}
      {fileCount === 0 && !dropError && (
        <p className="text-xs ml-1" style={{ color: "var(--text-muted)" }}>
          {t("timing_drop_hint")}
        </p>
      )}

      {/* Mode selector: Single Offset vs Timing Map (.json / .csv) */}
      <div className="flex flex-wrap items-center gap-4">
        <label
          className="flex items-center gap-2 text-sm cursor-pointer"
          style={{ color: "var(--text-primary)" }}
        >
          <input
            type="radio"
            name="timing-shift-mode"
            value="offset"
            checked={mode === "offset"}
            onChange={() => setMode("offset")}
            disabled={busy}
          />
          <span>{t("timing_mode_offset")}</span>
        </label>
        <label
          className="flex items-center gap-2 text-sm cursor-pointer"
          style={{ color: "var(--text-primary)" }}
        >
          <input
            type="radio"
            name="timing-shift-mode"
            value="map"
            checked={mode === "map"}
            onChange={() => setMode("map")}
            disabled={busy}
          />
          <span>{t("timing_mode_map")}</span>
        </label>
      </div>

      {mode === "offset" ? (
        <>
          {/* Offset value + unit */}
          <div className="flex items-end gap-3">
            <div>
              <label
                htmlFor="timing-offset-input"
                className="block text-sm font-medium mb-1"
                style={{ color: "var(--text-primary)" }}
              >
                {t("offset_label")}
              </label>
              <NumberInput
                id="timing-offset-input"
                value={offsetText}
                onChange={handleOffsetChange}
                min={0}
                max={offsetMax}
                step={unit === "ms" ? 1 : "0.001"}
                disabled={busy}
                invalid={offsetInvalid}
                className="w-28"
              />
            </div>
            <select
              id="timing-unit-select"
              name="offset-unit"
              aria-label={t("offset_label")}
              value={unit}
              onChange={(e) => handleUnitChange(e.target.value as Unit)}
              disabled={busy}
              className="px-3 py-2 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-[var(--accent)]"
              style={{
                background: "var(--bg-input)",
                border: "1px solid var(--border)",
                color: "var(--text-primary)",
              }}
            >
              <option value="ms">{t("unit_ms")}</option>
              <option value="s">{t("unit_seconds")}</option>
            </select>
          </div>

          {/* Direction picker — two big buttons with arrow glyphs */}
          <div className="dir-picker" role="radiogroup" aria-label={t("offset_label")}>
            <button
              type="button"
              className="dir-btn"
              role="radio"
              aria-checked={direction === "faster"}
              onClick={() => setDirection("faster")}
              disabled={busy}
            >
              <span className="dir-arrow" aria-hidden="true">
                ←
              </span>
              <span className="dir-label">{t("direction_faster")}</span>
            </button>
            <button
              type="button"
              className="dir-btn"
              role="radio"
              aria-checked={direction === "slower"}
              onClick={() => setDirection("slower")}
              disabled={busy}
            >
              <span className="dir-label">{t("direction_slower")}</span>
              <span className="dir-arrow" aria-hidden="true">
                →
              </span>
            </button>
          </div>

          <p className="text-xs" style={{ color: "var(--text-muted)", marginTop: "-0.25rem" }}>
            {t("offset_hint")}
          </p>

          {/* Threshold */}
          <div className="flex items-center gap-3">
            <label
              className="flex items-center gap-2 text-sm cursor-pointer"
              style={{ color: "var(--text-primary)" }}
            >
              <input
                type="checkbox"
                id="timing-threshold-checkbox"
                name="threshold-enabled"
                checked={useThreshold}
                onChange={(e) => setUseThreshold(e.target.checked)}
                disabled={busy}
                className="rounded"
                style={{
                  background: "var(--bg-input)",
                  borderColor: "var(--border)",
                }}
              />
              {t("threshold_label")}
            </label>
            {useThreshold && (
              <input
                type="text"
                id="timing-threshold-input"
                name="threshold"
                value={thresholdText}
                onChange={(e) => setThresholdText(e.target.value)}
                disabled={busy}
                placeholder="00:05:00.000"
                className="w-40 px-3 py-1.5 rounded-lg text-sm font-mono focus:outline-none focus:ring-2 focus:ring-[var(--accent)]"
                style={{
                  background: "var(--bg-input)",
                  border: "1px solid var(--border)",
                  color: "var(--text-primary)",
                }}
              />
            )}
            {thresholdInvalid && (
              <span className="text-xs" style={{ color: "var(--error)" }}>
                {t("threshold_invalid")}
              </span>
            )}
            {!thresholdInvalid && thresholdExceedsFile && (
              <span className="text-xs" style={{ color: "var(--warning)" }}>
                {t("threshold_exceeds_file")}
              </span>
            )}
          </div>
          {useThreshold && (
            <p
              className="text-xs"
              style={{
                color: "var(--text-muted)",
                marginTop: "-0.5rem",
                whiteSpace: "normal",
                overflowWrap: "anywhere",
              }}
            >
              {t("threshold_format_hint")}
            </p>
          )}
        </>
      ) : (
        <div className="space-y-3">
          {/* Timing-map file strip */}
          <div className="flex items-center gap-2">
            <div
              className="flex-1 min-w-0 flex items-center gap-2 px-3 rounded-lg text-sm"
              style={{
                background: timingMap ? "var(--bg-panel)" : "var(--bg-input)",
                border: "1px solid var(--border-light)",
                minHeight: "38px",
              }}
            >
              {timingMap ? (
                <>
                  <span
                    className="truncate flex-1"
                    style={{ color: "var(--text-primary)" }}
                    title={sanitizeForDialog(timingMap.filePath)}
                  >
                    {timingMap.fileName}
                  </span>
                  <span
                    className="flex-none px-2 py-0.5 rounded text-xs"
                    style={{
                      background: "var(--bg-input)",
                      color: "var(--text-secondary)",
                    }}
                  >
                    {t(
                      "timing_map_rules_summary",
                      timingMap.activeRuleCount,
                      timingMap.rules.length
                    )}
                  </span>
                </>
              ) : (
                <span className="italic" style={{ color: "var(--text-muted)" }}>
                  {t("timing_map_empty")}
                </span>
              )}
            </div>
            {timingMap && (
              <button
                type="button"
                onClick={handleClearTimingMap}
                disabled={busy}
                className="flex-none px-3 rounded-lg text-lg font-bold transition-colors"
                style={{
                  background: busy ? "var(--bg-input)" : "var(--cancel-bg)",
                  color: busy ? "var(--text-muted)" : "var(--cancel-text)",
                  height: "38px",
                }}
                title={t("btn_clear_timing_map")}
              >
                ✕
              </button>
            )}
            <button
              type="button"
              onClick={handlePickTimingMap}
              disabled={busy}
              className="flex-none px-5 rounded-lg font-medium text-sm transition-colors"
              style={{
                background: busy ? "var(--bg-input)" : "var(--accent)",
                color: busy ? "var(--text-muted)" : "var(--accent-text)",
                height: "38px",
              }}
            >
              {t("btn_select_timing_map")}
            </button>
          </div>

          <p className="text-xs ml-1" style={{ color: "var(--text-muted)" }}>
            {t("timing_map_hint")}
          </p>

          {/* Read-only parsed rules summary */}
          {timingMap && (
            <div
              className="rounded-lg overflow-hidden"
              style={{
                border: "1px solid var(--border)",
                background: "var(--bg-panel)",
              }}
            >
              <div className="px-3 py-2" style={{ borderBottom: "1px solid var(--border)" }}>
                <span className="text-xs font-medium" style={{ color: "var(--text-muted)" }}>
                  {t("timing_map_rules_title", timingMap.activeRuleCount, timingMap.rules.length)}
                </span>
              </div>
              <div
                className="grid items-center gap-3 px-3 py-1.5 text-xs font-medium"
                style={{
                  gridTemplateColumns: "36px minmax(180px, 1.4fr) 110px minmax(100px, 1fr)",
                  borderBottom: "1px solid var(--border)",
                  color: "var(--text-muted)",
                }}
                aria-hidden="true"
              >
                <span>{t("col_index")}</span>
                <span>{t("timing_map_col_range")}</span>
                <span>{t("timing_map_col_offset")}</span>
                <span>{t("timing_map_col_label")}</span>
              </div>
              <div className="max-h-44 overflow-y-auto">
                {timingMap.rules.map((rule, idx) => {
                  const enabled = rule.enabled !== false;
                  const startLabel = formatDisplayTime(rule.startMs);
                  const rangeLabel =
                    rule.endMs !== undefined
                      ? t("timing_map_range_bounded", startLabel, formatDisplayTime(rule.endMs))
                      : t("timing_map_range_open", startLabel);
                  const safeLabel = rule.label ? sanitizeForDialog(rule.label) : null;
                  return (
                    <div
                      key={idx}
                      className="grid items-center gap-3 px-3 py-1.5 text-xs"
                      style={{
                        gridTemplateColumns: "36px minmax(180px, 1.4fr) 110px minmax(100px, 1fr)",
                        borderBottom:
                          idx < timingMap.rules.length - 1
                            ? "1px solid color-mix(in srgb, var(--border) 50%, transparent)"
                            : "none",
                        color: enabled ? "var(--text-primary)" : "var(--text-muted)",
                      }}
                    >
                      <span className="font-mono">{idx + 1}</span>
                      <span className="font-mono truncate" title={rangeLabel}>
                        {rangeLabel}
                      </span>
                      <span className="font-mono">{formatSignedOffsetMs(rule.offsetMs)}</span>
                      <span className="flex items-center gap-2 min-w-0">
                        {safeLabel && (
                          <span className="truncate" title={safeLabel}>
                            {safeLabel}
                          </span>
                        )}
                        <span className={enabled ? "badge badge-green" : "badge badge-mute"}>
                          {enabled ? t("timing_map_rule_active") : t("timing_map_rule_disabled")}
                        </span>
                      </span>
                    </div>
                  );
                })}
              </div>
            </div>
          )}
        </div>
      )}

      {/* Timeline preview — shows the FIRST file's captions in batch mode */}
      {preview.length > 0 && (
        <div className="timeline-preview" aria-live="polite">
          <div className="timeline-preview-head">
            <span>
              {previewTruncated
                ? fileCount > 1
                  ? t(
                      "preview_title_first_truncated",
                      preview.length,
                      shiftableCount,
                      primaryFileName
                    )
                  : t("preview_title_truncated", preview.length, shiftableCount)
                : fileCount > 1
                  ? t("preview_title_first", shiftableCount, primaryFileName)
                  : t("preview_title", shiftableCount)}
            </span>
          </div>
          <div className="timeline-row timeline-row-header" aria-hidden="true">
            <span>{t("col_index")}</span>
            <span>{t("col_original")}</span>
            <span>{t("col_shifted")}</span>
            <span>{t("col_text")}</span>
          </div>
          <List<PreviewRowData>
            className="timeline-list"
            style={previewListStyle}
            rowCount={preview.length}
            rowHeight={PREVIEW_ROW_HEIGHT}
            rowComponent={PreviewRow}
            rowProps={previewRowProps}
          />
        </div>
      )}

      {/* Log */}
      <LogPanel logs={logs} onClear={clearLogs} scrollRef={logScrollRef} />
    </div>
  );
}

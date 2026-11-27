import FontSourceModal from "./FontSourceModal";
import { LogPanel } from "../../lib/LogPanel";
import { DropErrorBanner } from "../../lib/DropErrorBanner";
import { renderSubtitleFileStrip } from "../../lib/subtitle-file-strip";
import { sanitizeForDialog } from "../../lib/dedup-helpers";
import {
  fontSelectionKey,
  useFontEmbedController,
  type FontEmbedProps,
} from "./useFontEmbedController";

export default function FontEmbed(props: FontEmbedProps) {
  const {
    t,
    filePaths,
    fileNames,
    fileCount,
    isSingleFile,
    isBatch,
    showFileList,
    setShowFileList,
    dropZoneRef,
    fileContainerRef,
    dropActive,
    dropError,
    setDropError,
    cacheUnavailable,
    outputMode,
    setOutputMode,
    chosenOutputDir,
    outputControlsDisabled,
    missingChosenOutputDir,
    handlePickOutputDir,
    handlePickFiles,
    handleClearFiles,
    analyzing,
    embedding,
    sourceLocked,
    sourceBusy,
    fontSources,
    fontSourceEntryCount,
    sourceModalOpen,
    setSourceModalOpen,
    canStartSourceMutation,
    handleClearFontSources,
    handleAddFontSource,
    handleRemoveFontSource,
    handleScanStateChange,
    embedCancelRequested,
    requestEmbedCancel,
    handleEmbed,
    isEmbedDisabled,
    embedButtonLabel,
    fonts,
    fontUsages,
    localCoveredKeys,
    selected,
    toggleSelect,
    formatFontLabel,
    progress,
    logs,
    clearLogs,
    logScrollRef,
  } = useFontEmbedController(props);

  return (
    <div className="space-y-4">
      {/* File strip — drop zone + filename(s) + clear + Select */}
      {renderSubtitleFileStrip({
        dropZoneRef,
        fileContainerRef,
        dropActive,
        filePaths,
        fileNames,
        showFileList,
        onToggleFileList: () => setShowFileList((v) => !v),
        emptyLabel: t("file_empty"),
        selectedTitle: t("files_selected_title", fileCount),
        clearTitle: t("btn_clear_file"),
        selectLabel: analyzing ? t("btn_analyzing") : t("btn_select_files"),
        clearDisabled: analyzing || embedding,
        selectDisabled: analyzing || embedding || sourceLocked,
        onClearFiles: handleClearFiles,
        onSelectFiles: handlePickFiles,
      })}

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
                color: outputControlsDisabled ? "var(--text-muted)" : "var(--accent-text)",
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
          disabled={embedding || analyzing || sourceLocked}
          className="px-5 rounded-lg font-medium text-sm transition-colors"
          style={
            embedding || analyzing || sourceLocked
              ? {
                  background: "var(--accent-disabled-bg)",
                  color: "var(--accent-disabled-text)",
                  height: "38px",
                }
              : { background: "var(--accent)", color: "var(--accent-text)", height: "38px" }
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
            disabled={embedding || analyzing || sourceLocked}
            className="flex-none px-3 rounded-lg text-lg font-bold transition-colors"
            style={{
              background:
                embedding || analyzing || sourceLocked ? "var(--bg-input)" : "var(--cancel-bg)",
              color:
                embedding || analyzing || sourceLocked ? "var(--text-muted)" : "var(--cancel-text)",
              height: "38px",
            }}
            // Mirror the Modal's per-row remove tooltip pattern: when
            // disabled-by-busy, the tooltip should match the disabled
            // state instead of the action label that's not currently
            // available.
            title={
              embedding
                ? t("btn_embedding")
                : analyzing
                  ? t("btn_analyzing")
                  : sourceLocked
                    ? t("font_sources_scanning")
                    : t("btn_clear_font_sources")
            }
          >
            ✕
          </button>
        )}
        <div className="flex-1" />
        {embedding && (
          <button
            onClick={requestEmbedCancel}
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
              : {
                  background: "var(--accent)",
                  color: "var(--accent-text)",
                  height: "38px",
                  minWidth: "140px",
                }
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
                    <span
                      className={"badge " + (info.filePath ? "badge-green" : "badge-red")}
                      title={info.error ?? undefined}
                      aria-label={
                        info.filePath
                          ? t("fonts_found")
                          : info.error
                            ? `${t("fonts_missing")}: ${info.error}`
                            : t("fonts_missing")
                      }
                    >
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

      {/* The modal receives the same aggregated usage union as the grid,
          so its coverage statistics represent the whole selection. */}
      <FontSourceModal
        open={sourceModalOpen}
        onClose={() => setSourceModalOpen(false)}
        sources={fontSources}
        usages={fontUsages}
        localCoveredKeys={localCoveredKeys}
        hasSubtitle={fileCount > 0}
        mutationLocked={analyzing || embedding || sourceBusy}
        canStartMutation={canStartSourceMutation}
        onAddSource={handleAddFontSource}
        onRemoveSource={handleRemoveFontSource}
        onScanStateChange={handleScanStateChange}
      />
    </div>
  );
}

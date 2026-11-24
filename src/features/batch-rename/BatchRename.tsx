/**
 * Batch Rename — Tab 4.
 *
 * Tab plumbing + ingestion (file pick / folder drop, auto
 * categorization, count chips, cross-tab dedup). Pairing engine +
 * preview grid. Output-mode radios (rename / copy-to-video /
 * copy-to-chosen) + per-row checkbox toggle + run flow with
 * rename-confirm + existing-output overwrite-confirm + cancel
 * mid-batch. Manual edit: video-centric grid — exactly one row per
 * video. The subtitle column is a dropdown listing every subtitle
 * in the batch; the first regex-paired sub is pre-selected. The
 * user re-pairs by picking a different sub; subs already paired
 * with another video are unpaired automatically (uniquely owned).
 * Subtitles whose episode regex didn't match any video are hidden
 * from the grid but stay in the dropdown — the workflow is
 * video-first ("I have a video, find me a sub for it"). Multi-subtitle
 * mode switches the seed to one row per matched subtitle for the same
 * video, preserving language suffixes on output. ↺ Reset restores the
 * engine's seed.
 */
import { useMemo } from "react";
import { SubtitlePicker } from "./SubtitlePicker";
import { PreviewTable, type PreviewTableColumn } from "../../lib/PreviewTable";
import { LogPanel } from "../../lib/LogPanel";
import { DropErrorBanner } from "../../lib/DropErrorBanner";
import { sanitizeForDialog } from "../../lib/dedup-helpers";
import { type PairingRow, type PairingSource } from "./pairing-engine";
import { useBatchRenameController } from "./useBatchRenameController";

function renderSourceBadge(
  source: PairingSource,
  t: (key: string, ...args: (string | number)[]) => string
) {
  const labels: Record<PairingSource, string> = {
    regex: "rename_source_regex",
    lcs: "rename_source_lcs",
    manual: "rename_source_manual",
    unmatched: "rename_source_unmatched",
    warning: "rename_source_warning",
  };
  return (
    <span className={`badge ${source === "warning" ? "badge-warning" : "badge-mute"}`}>
      {t(labels[source])}
    </span>
  );
}

export default function BatchRename() {
  const {
    t,
    busy,
    dropZoneRef,
    dropActive,
    dropError,
    setDropError,
    videoCount,
    subtitleCount,
    totalCount,
    unknownCount,
    outputMode,
    setOutputMode,
    chosenDir,
    multiSubtitleMode,
    setMultiSubtitleMode,
    pairingRows,
    availableSubtitles,
    activePickerRow,
    setActivePickerRow,
    toggleRow,
    assignSubtitleLocal,
    resetPairings,
    hasManualEdits,
    warningCount,
    unpairedSubtitleCount,
    actionableCount,
    handlePickFiles,
    handleClearFiles,
    handlePickChosenDir,
    handleRunRename,
    requestRenameCancel,
    logs,
    clearLogs,
    logScrollRef,
  } = useBatchRenameController();

  const pairingColumns = useMemo<PreviewTableColumn<PairingRow>[]>(
    () => [
      {
        key: "select",
        header: "",
        width: "28px",
        render: (row) => {
          // Checkbox is only meaningful for rows with both video AND
          // subtitle — orphan rows have nothing to write, so the
          // checkbox is hidden for those.
          const canSelect = row.video !== null && row.subtitle !== null;
          if (!canSelect) return null;
          return (
            <input
              type="checkbox"
              checked={row.selected}
              disabled={busy}
              onChange={() => toggleRow(row.id)}
              aria-label={t("rename_row_select_aria")}
            />
          );
        },
      },
      {
        key: "idx",
        header: t("col_index"),
        width: "32px",
        render: (_row, i) => i + 1,
      },
      {
        key: "video",
        header: t("rename_col_video"),
        width: "1fr",
        render: (row) =>
          row.video ? (
            // Sanitize the visible video filename before the
            // destructive-rename ask() dialog. A BiDi-reversed display
            // in the grid undermines the visual-confirm value the user
            // relies on BEFORE the dialog appears (they pick which row
            // to rename from this grid, not the dialog).
            <span title={sanitizeForDialog(row.video.name)}>
              {sanitizeForDialog(row.video.name)}
            </span>
          ) : (
            <span style={{ color: "var(--text-muted)", fontStyle: "italic" }}>—</span>
          ),
      },
      {
        key: "sub",
        header: t("rename_col_subtitle"),
        width: "1fr",
        render: (row) => {
          // Empty-video orphan rows (no video either — shouldn't
          // happen in the video-centric grid, but render a dash
          // defensively rather than an interactable dropdown).
          if (!row.video) {
            return <span style={{ color: "var(--text-muted)", fontStyle: "italic" }}>—</span>;
          }
          // Universal dropdown: always render, regardless of whether
          // the row currently has a sub. Picking the empty option
          // unpairs; picking a sub assigns it (and unpairs that sub
          // from any other row that had it). The select is disabled
          // when there are no subs at all in the batch — otherwise
          // it would be useless and the muted "(none)" placeholder
          // would mislead.
          return (
            <SubtitlePicker
              rowId={row.id}
              subtitle={row.subtitle}
              choices={availableSubtitles}
              active={activePickerRow === row.id}
              disabled={busy}
              label={t("rename_pick_subtitle")}
              emptyLabel={t("rename_pick_subtitle_none")}
              onActivate={() => setActivePickerRow(row.id)}
              onDeactivate={() =>
                setActivePickerRow((current) => (current === row.id ? null : current))
              }
              onAssign={(path) => assignSubtitleLocal(row.id, path)}
            />
          );
        },
      },
      {
        key: "source",
        header: t("rename_col_source"),
        width: "100px",
        render: (row) => renderSourceBadge(row.source, t),
      },
    ],
    [
      t,
      busy,
      toggleRow,
      assignSubtitleLocal,
      availableSubtitles,
      activePickerRow,
      setActivePickerRow,
    ]
  );

  return (
    <div className="space-y-4">
      {/* File strip / drop zone — the only ingest surface for this tab.
           Videos and subtitles auto-categorize after the drop / pick.
           Drop hint surfaces only when idle. */}
      <div
        ref={dropZoneRef}
        className={`drop-zone flex items-center gap-2${dropActive ? " drop-active" : ""}`}
      >
        <div
          className="flex-1 min-w-0 flex items-center gap-2 px-3 rounded-lg text-sm"
          style={{
            background: totalCount > 0 ? "var(--bg-panel)" : "var(--bg-input)",
            border: "1px solid var(--border-light)",
            minHeight: "38px",
          }}
        >
          {totalCount > 0 ? (
            <>
              <span
                className="flex-none px-2 py-0.5 rounded text-xs"
                style={{
                  background: "var(--bg-input)",
                  color: "var(--text-primary)",
                }}
              >
                {t("rename_chip_videos", videoCount)}
              </span>
              <span
                className="flex-none px-2 py-0.5 rounded text-xs"
                style={{
                  background: "var(--bg-input)",
                  color: "var(--text-primary)",
                }}
              >
                {t("rename_chip_subtitles", subtitleCount)}
              </span>
              {unknownCount > 0 && (
                <span
                  className="flex-none px-2 py-0.5 rounded text-xs"
                  style={{
                    background: "var(--bg-input)",
                    color: "var(--text-muted)",
                  }}
                  title={t("rename_chip_unknown_hint")}
                >
                  {t("rename_chip_unknown", unknownCount)}
                </span>
              )}
              <span className="flex-1" />
            </>
          ) : (
            <span className="italic" style={{ color: "var(--text-muted)" }}>
              {t("file_empty")}
            </span>
          )}
        </div>
        {totalCount > 0 && (
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
          {t("btn_select_rename_inputs")}
        </button>
        {busy && (
          <button
            onClick={requestRenameCancel}
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
        {(() => {
          // copy_to_chosen requires a chosen output directory; without
          // one the click handler logs an error and returns. Make that
          // visible in the button state instead — disable + tooltip
          // explains why, mirroring the gate other tabs use for
          // similar prerequisite-state rules.
          const missingChosenDir = outputMode === "copy_to_chosen" && !chosenDir;
          const disabled = busy || actionableCount === 0 || missingChosenDir;
          return (
            <button
              onClick={handleRunRename}
              disabled={disabled}
              title={missingChosenDir ? t("msg_rename_no_chosen_dir") : undefined}
              className="flex-none px-6 rounded-lg font-medium text-sm transition-colors"
              style={
                disabled
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
              {busy ? t("btn_renaming") : t("btn_rename_run", actionableCount)}
            </button>
          );
        })()}
      </div>

      <DropErrorBanner message={dropError} onDismiss={() => setDropError(null)} />

      {totalCount === 0 && !dropError && (
        <p className="text-xs ml-1" style={{ color: "var(--text-muted)" }}>
          {t("rename_drop_hint")}
        </p>
      )}
      {totalCount > 0 && (
        <p className="text-xs ml-1" style={{ color: "var(--text-muted)" }}>
          {t("rename_manual_edit_hint")}
        </p>
      )}

      {/* Output-mode strategy. Three modes
           with a chosen-dir picker visible only when the third mode is
           selected. The mode persists across selection changes; the
           chosen-dir is cleared when files clear. Styled as a plain
           div+heading rather than fieldset+legend — the browser default
           legend-on-border styling reads as a layout bug. Per-input
           disabled={busy} on each radio gives the same gating fieldset
           did. */}
      {totalCount > 0 && (
        <div
          className="rounded-lg px-4 py-3"
          style={{ border: "1px solid var(--border-light)", background: "var(--bg-panel)" }}
        >
          <div className="text-xs font-medium mb-2" style={{ color: "var(--text-muted)" }}>
            {t("rename_mode_label")}
          </div>
          <div className="flex flex-col gap-2">
            <label
              className="flex items-center gap-2 text-sm cursor-pointer"
              style={{ color: "var(--text-primary)" }}
            >
              <input
                type="checkbox"
                checked={multiSubtitleMode}
                onChange={(e) => setMultiSubtitleMode(e.target.checked)}
                disabled={busy}
              />
              <span>{t("rename_multi_subtitle_mode")}</span>
            </label>
            <div className="text-xs ml-6" style={{ color: "var(--text-muted)" }}>
              {t("rename_multi_subtitle_mode_hint")}
            </div>
            <label
              className="flex items-center gap-2 text-sm cursor-pointer"
              style={{ color: "var(--text-primary)" }}
            >
              <input
                type="radio"
                name="rename-mode"
                value="copy_to_video"
                checked={outputMode === "copy_to_video"}
                onChange={() => setOutputMode("copy_to_video")}
                disabled={busy}
              />
              <span>{t("rename_mode_copy_to_video")}</span>
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
                name="rename-mode"
                value="copy_to_chosen"
                checked={outputMode === "copy_to_chosen"}
                onChange={() => setOutputMode("copy_to_chosen")}
                disabled={busy}
              />
              <span>{t("rename_mode_copy_to_chosen")}</span>
            </label>
            {outputMode === "copy_to_chosen" && (
              <div className="flex items-center gap-2 ml-6">
                <button
                  onClick={handlePickChosenDir}
                  disabled={busy}
                  className="px-3 py-1 rounded text-xs font-medium"
                  style={{
                    background: busy ? "var(--bg-input)" : "var(--accent)",
                    color: busy ? "var(--text-muted)" : "var(--accent-text)",
                  }}
                >
                  {t("btn_pick_chosen_dir")}
                </button>
                {chosenDir ? (
                  <span
                    className="text-xs truncate flex-1"
                    style={{ color: "var(--text-secondary)" }}
                    title={sanitizeForDialog(chosenDir)}
                  >
                    {sanitizeForDialog(chosenDir)}
                  </span>
                ) : (
                  <span className="text-xs italic" style={{ color: "var(--text-muted)" }}>
                    {t("rename_chosen_dir_empty")}
                  </span>
                )}
              </div>
            )}
            <label
              className="flex items-center gap-2 text-sm cursor-pointer"
              style={{ color: "var(--text-primary)" }}
            >
              <input
                type="radio"
                name="rename-mode"
                value="rename"
                checked={outputMode === "rename"}
                onChange={() => setOutputMode("rename")}
                disabled={busy}
              />
              <span>{t("rename_mode_in_place")}</span>
              <span className="text-xs" style={{ color: "var(--warning)" }}>
                {t("rename_mode_in_place_hint")}
              </span>
            </label>
          </div>
        </div>
      )}

      {/* Pairing preview grid. Video-centric: one row per video, with
           the first regex-paired sub pre-selected. The subtitle column
           is a universal dropdown — pick a different sub to re-pair,
           pick "— none —" to unpair. Subtitles whose episode regex
           didn't match any video aren't given their own row but stay
           in the dropdown options (the workflow is "find a sub for
           this video", not the other way around). Source badge tells
           the user which algorithm decided the pair; rows the user
           edited flip to `manual`. ↺ Reset surfaces only when any row
           has a manual edit. */}
      {totalCount > 0 && (
        <PreviewTable
          rows={pairingRows}
          rowKey={(row) => row.id}
          columns={pairingColumns}
          title={
            <div className="flex items-center gap-2">
              <span>{t("rename_grid_title", pairingRows.length)}</span>
              {warningCount > 0 && (
                <span style={{ color: "var(--warning)" }}>
                  {" · "}
                  {t("rename_grid_warning_suffix", warningCount)}
                </span>
              )}
              {unpairedSubtitleCount > 0 && (
                <span style={{ color: "var(--warning)" }}>
                  {t("rename_grid_unpaired_subtitle_suffix", unpairedSubtitleCount)}
                </span>
              )}
              <span className="flex-1" />
              {hasManualEdits && (
                <button
                  type="button"
                  onClick={resetPairings}
                  disabled={busy}
                  className="rename-reset-button"
                  title={t("rename_reset_pairings_hint")}
                >
                  ↺ {t("rename_reset_pairings")}
                </button>
              )}
            </div>
          }
          emptyMessage={t("rename_no_pairings")}
          rowClassName={(row) => (row.source === "warning" ? "preview-row-warning" : undefined)}
        />
      )}

      {/* Log */}
      <LogPanel logs={logs} onClear={clearLogs} scrollRef={logScrollRef} />
    </div>
  );
}

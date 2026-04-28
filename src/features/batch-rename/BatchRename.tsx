/**
 * Batch Rename — Tab 4.
 *
 * Stage 5a: tab plumbing + ingestion (file pick / folder drop, auto
 *   categorization, count chips, cross-tab dedup).
 * Stage 5b: pairing engine + preview grid.
 * Stage 5c: output-mode radios (rename / copy-to-video / copy-to-chosen)
 *   + per-row checkbox toggle + run flow with rename-confirm +
 *   countExistingFiles overwrite-confirm + cancel mid-batch.
 * Stage 5d: manual edit. Video-centric grid — exactly one row per
 *   video. The subtitle column is a dropdown listing every subtitle
 *   in the batch; the first regex-paired sub is pre-selected. The
 *   user re-pairs by picking a different sub; subs already paired
 *   with another video are unpaired automatically (uniquely owned).
 *   Subtitles whose episode regex didn't match any video are hidden
 *   from the grid but stay in the dropdown — the workflow is
 *   video-first ("I have a video, find me a sub for it"). ↺ Reset
 *   restores the engine's seed.
 */
import { useState, useCallback, useEffect, useRef, useMemo } from "react";
import {
  pickRenameInputs,
  pickOutputDirectory,
  renamePath,
  copyPath,
  fileNameFromPath,
} from "../../lib/tauri-api";
import { ask } from "@tauri-apps/plugin-dialog";
import { useI18n } from "../../i18n/useI18n";
import { useFileContext } from "../../lib/FileContext";
import { TAB_LABEL_KEYS } from "../../lib/tab-labels";
import type { TabId } from "../../lib/FileContext";
import type { Status } from "../../lib/StatusContext";
import { useTabStatus } from "../../lib/useTabStatus";
import { useFolderDrop } from "../../lib/useFolderDrop";
import { PreviewTable, type PreviewTableColumn } from "../../lib/PreviewTable";
import { countExistingFiles } from "../../lib/output-collisions";
import {
  buildPairings,
  parseFilename,
  deriveRenameOutputPath,
  isNoOpRename,
  assignSubtitleToRow,
  type PairingRow,
  type PairingSource,
  type OutputMode,
} from "./pairing-engine";

interface LogEntry {
  id: number;
  text: string;
  type: "info" | "error" | "success";
}

// Categorization sets — drive both the picker filter (in tauri-api) and
// the post-drop classification here. Kept in sync manually for now;
// extracting to a shared constant could be a Stage 5c cleanup.
const VIDEO_EXTS = new Set([
  "mp4",
  "mkv",
  "avi",
  "mov",
  "ts",
  "m2ts",
  "webm",
  "flv",
  "wmv",
  "mpg",
  "mpeg",
  "m4v",
]);
const SUBTITLE_EXTS = new Set(["ass", "ssa", "srt", "sub", "vtt", "sbv", "lrc"]);

// Extensions we intentionally drop on the floor during folder ingestion.
// These are companions that ship in fan-sub release folders but have no
// place in this app's workflow — surfacing them in the unknown counter
// would be noise, not signal. Categories:
//   - source / metadata       : torrent
//   - common archive formats  : zip, rar, 7z, tar, gz, bz2, xz, tgz
//   - companion audio tracks  : mka, flac, mp3, m4a, aac (e.g., separate
//                                audio supplied alongside an HEVC video)
// Add to this set when a release-folder staple shows up that can never
// be a Tab 4 input.
const IGNORED_EXTS = new Set([
  "torrent",
  "zip",
  "rar",
  "7z",
  "tar",
  "gz",
  "bz2",
  "xz",
  "tgz",
  "mka",
  "flac",
  "mp3",
  "m4a",
  "aac",
]);

type Category = "video" | "subtitle" | "ignored" | "unknown";

function categorize(name: string): Category {
  const dot = name.lastIndexOf(".");
  if (dot < 0) return "unknown";
  const ext = name.slice(dot + 1).toLowerCase();
  if (VIDEO_EXTS.has(ext)) return "video";
  if (SUBTITLE_EXTS.has(ext)) return "subtitle";
  if (IGNORED_EXTS.has(ext)) return "ignored";
  return "unknown";
}

interface Categorized {
  videos: string[];
  subtitles: string[];
  unknown: string[];
}

function categorizePaths(paths: string[]): Categorized {
  const videos: string[] = [];
  const subtitles: string[] = [];
  const unknown: string[] = [];
  for (const p of paths) {
    const cat = categorize(fileNameFromPath(p));
    if (cat === "video") videos.push(p);
    else if (cat === "subtitle") subtitles.push(p);
    else if (cat === "ignored") continue;
    else unknown.push(p);
  }
  return { videos, subtitles, unknown };
}

// Source badge — color-coded chip per pairing's `source` field. Uses
// existing palette tokens (success / warning / muted / accent) so the
// theme swap stays consistent across light/dark.
function renderSourceBadge(
  source: PairingSource,
  t: (key: string, ...args: (string | number)[]) => string
): JSX.Element {
  const map: Record<PairingSource, { labelKey: string; color: string; bg: string }> = {
    regex: {
      labelKey: "rename_source_regex",
      color: "var(--success)",
      bg: "var(--badge-green-bg)",
    },
    lcs: {
      labelKey: "rename_source_lcs",
      color: "var(--text-muted)",
      bg: "var(--bg-input)",
    },
    manual: {
      labelKey: "rename_source_manual",
      color: "var(--accent)",
      bg: "var(--bg-input)",
    },
    unmatched: {
      labelKey: "rename_source_unmatched",
      color: "var(--text-muted)",
      bg: "transparent",
    },
    warning: {
      labelKey: "rename_source_warning",
      color: "var(--warning)",
      bg: "color-mix(in srgb, var(--warning) 15%, transparent)",
    },
  };
  const { labelKey, color, bg } = map[source];
  return (
    <span
      style={{
        display: "inline-block",
        padding: "1px 8px",
        borderRadius: "4px",
        fontSize: "10px",
        fontWeight: 600,
        letterSpacing: "0.04em",
        color,
        background: bg,
      }}
    >
      {t(labelKey)}
    </span>
  );
}

export default function BatchRename() {
  const { t } = useI18n();
  const { renameFiles, setRenameFiles, clearFile, isFileInUse } = useFileContext();

  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<{ processed: number; total: number } | null>(null);
  const [lastActionResult, setLastActionResult] = useState<
    "success" | "error" | "cancelled" | "noop" | null
  >(null);
  const [dropActive, setDropActive] = useState(false);
  const [dropError, setDropError] = useState<string | null>(null);
  // Unknown-extension counter — surfaced in the chips but not in
  // renameFiles state, since the unknown bucket isn't pairing material.
  // Reset whenever renameFiles changes.
  const [unknownCount, setUnknownCount] = useState(0);
  const [logs, setLogs] = useState<LogEntry[]>([]);
  // Output strategy — three modes per design doc 已决定 #3.
  // Default `copy_to_video` matches the most common fan-sub workflow
  // (subs end up in the same folder as the videos, originals untouched).
  const [outputMode, setOutputMode] = useState<OutputMode>("copy_to_video");
  // Picked target directory for the `copy_to_chosen` mode. Required
  // before Run when that mode is active.
  const [chosenDir, setChosenDir] = useState<string | null>(null);
  // Pairing rows are direct state, not a derived view over an
  // overrides Map. The engine seeds them from the input file lists;
  // user actions (toggle / dropdown pick) mutate rows in place,
  // marking edited rows as `source: 'manual'`. ↺ Reset restores the
  // engine's seed.
  const [editedRows, setEditedRows] = useState<PairingRow[]>([]);

  const pickGenRef = useRef(0);
  const logIdRef = useRef(0);
  const cancelRef = useRef(false);
  const dropZoneRef = useRef<HTMLDivElement>(null);
  const logScrollRef = useRef<HTMLDivElement>(null);

  const videoPaths = useMemo(() => renameFiles?.videoPaths ?? [], [renameFiles]);
  const videoNames = useMemo(() => renameFiles?.videoNames ?? [], [renameFiles]);
  const subtitlePaths = useMemo(() => renameFiles?.subtitlePaths ?? [], [renameFiles]);
  const subtitleNames = useMemo(() => renameFiles?.subtitleNames ?? [], [renameFiles]);
  const videoCount = videoPaths.length;
  const subtitleCount = subtitlePaths.length;
  const totalCount = videoCount + subtitleCount;

  // Pairing seed — recomputed whenever the file lists change. Each
  // row gets a fresh stable ID with a `b<n>` prefix (counter-local to
  // this useMemo) so subsequent manual edits can reference rows by ID
  // even when the row's subtitle changes. The engine's content-based
  // IDs would shift the moment the user picks a different sub.
  const baseRows = useMemo<PairingRow[]>(() => {
    const parsedVideos = videoPaths.map((p, i) => parseFilename(p, videoNames[i] ?? ""));
    const parsedSubs = subtitlePaths.map((p, i) => parseFilename(p, subtitleNames[i] ?? ""));
    let counter = 0;
    return buildPairings(parsedVideos, parsedSubs).map((r) => ({
      ...r,
      id: `b${++counter}`,
    }));
  }, [videoPaths, videoNames, subtitlePaths, subtitleNames]);

  // Reset edits when the input file lists change. baseRows only
  // recomputes when the user re-picks / clears, so this isn't a
  // surprise — it's an explicit "start fresh" point.
  useEffect(() => {
    setEditedRows(baseRows);
  }, [baseRows]);

  const pairingRows = editedRows;

  const toggleRow = useCallback((rowId: string) => {
    setEditedRows((rows) =>
      rows.map((r) => (r.id === rowId ? { ...r, selected: !r.selected } : r))
    );
  }, []);

  // Subtitle pool for every row's dropdown. Built from the original
  // subtitle inputs (not from row state) so subs that aren't paired
  // with any row are still selectable — the user-supplied sub list
  // is the source of truth for what's available to pair.
  const availableSubtitles = useMemo(
    () => subtitlePaths.map((p, i) => ({ path: p, name: subtitleNames[i] ?? p })),
    [subtitlePaths, subtitleNames]
  );

  const assignSubtitleLocal = useCallback(
    (rowId: string, subPath: string | null) => {
      const sub = subPath ? availableSubtitles.find((s) => s.path === subPath) : null;
      // Defensive: caller should never pass an unknown path, but
      // guard against it rather than producing a broken row.
      if (subPath && !sub) return;
      setEditedRows((rows) => assignSubtitleToRow(rows, rowId, sub ?? null));
    },
    [availableSubtitles]
  );

  const resetPairings = useCallback(() => {
    setEditedRows(baseRows);
  }, [baseRows]);

  const hasManualEdits = useMemo(() => editedRows.some((r) => r.source === "manual"), [editedRows]);

  const warningCount = useMemo(
    () => pairingRows.filter((r) => r.source === "warning").length,
    [pairingRows]
  );

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
            <span title={row.video.name}>{row.video.name}</span>
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
          const currentValue = row.subtitle?.path ?? "";
          return (
            <select
              name={`subtitle-picker-${row.id}`}
              value={currentValue}
              disabled={busy || availableSubtitles.length === 0}
              onChange={(e) => {
                const path = e.target.value;
                assignSubtitleLocal(row.id, path === "" ? null : path);
              }}
              className={`rename-row-picker${row.subtitle ? " is-paired" : ""}`}
              aria-label={t("rename_pick_subtitle")}
              title={row.subtitle?.name}
            >
              <option value="">{t("rename_pick_subtitle_none")}</option>
              {availableSubtitles.map((s) => (
                <option key={s.path} value={s.path}>
                  {s.name}
                </option>
              ))}
            </select>
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
    [t, busy, toggleRow, assignSubtitleLocal, availableSubtitles]
  );

  const actionableRows = useMemo(
    () => pairingRows.filter((r) => r.selected && r.video !== null && r.subtitle !== null),
    [pairingRows]
  );
  const actionableCount = actionableRows.length;

  const addLog = useCallback((text: string, type: LogEntry["type"] = "info") => {
    const id = logIdRef.current++;
    setLogs((prev) => {
      const next = [...prev, { id, text, type }];
      return next.length > 200 ? next.slice(-200) : next;
    });
    setTimeout(() => {
      const el = logScrollRef.current;
      if (el) el.scrollTop = el.scrollHeight;
    }, 50);
  }, []);

  // Strict cross-tab dedup. Only SUBTITLES are checked against the
  // other tabs (HDR / Timing / Fonts) — videos are unique to Tab 4.
  const checkConflicts = useCallback(
    (subtitlePathsToCheck: string[]): string | null => {
      let conflictCount = 0;
      let conflictTab: TabId | null = null;
      for (const p of subtitlePathsToCheck) {
        const usedIn = isFileInUse(p, "rename");
        if (usedIn) {
          if (conflictTab === null) conflictTab = usedIn;
          conflictCount++;
        }
      }
      if (conflictTab === null) return null;
      return t("msg_dedup_blocked", conflictCount, t(TAB_LABEL_KEYS[conflictTab]));
    },
    [isFileInUse, t]
  );

  const ingestPaths = useCallback(
    (paths: string[], gen: number) => {
      const { videos, subtitles, unknown } = categorizePaths(paths);

      // Conflict check applies only to subtitles — videos can't collide
      // with another tab.
      const conflictMsg = checkConflicts(subtitles);
      if (conflictMsg) {
        setDropError(conflictMsg);
        return;
      }
      setDropError(null);

      if (videos.length === 0 && subtitles.length === 0) {
        addLog(t("msg_no_rename_inputs_in_drop"), "error");
        return;
      }

      if (gen !== pickGenRef.current) return;

      setRenameFiles({
        videoPaths: videos,
        videoNames: videos.map(fileNameFromPath),
        subtitlePaths: subtitles,
        subtitleNames: subtitles.map(fileNameFromPath),
      });
      setUnknownCount(unknown.length);
      if (unknown.length > 0) {
        addLog(t("msg_rename_unknown_skipped", unknown.length), "info");
      }
    },
    [checkConflicts, setRenameFiles, addLog, t]
  );

  const handlePickFiles = useCallback(async () => {
    const gen = (pickGenRef.current = pickGenRef.current + 1);
    const paths = await pickRenameInputs();
    if (gen !== pickGenRef.current) return;
    if (!paths || paths.length === 0) return;
    ingestPaths(paths, gen);
  }, [ingestPaths]);

  const handleDroppedPaths = useCallback(
    (paths: string[]) => {
      const gen = (pickGenRef.current = pickGenRef.current + 1);
      ingestPaths(paths, gen);
    },
    [ingestPaths]
  );

  useFolderDrop({
    ref: dropZoneRef,
    onPaths: handleDroppedPaths,
    onActiveChange: setDropActive,
    disabled: busy,
  });

  const handleClearFiles = useCallback(() => {
    pickGenRef.current = pickGenRef.current + 1;
    clearFile("rename");
    setUnknownCount(0);
    setDropError(null);
  }, [clearFile]);

  const handlePickChosenDir = useCallback(async () => {
    const dir = await pickOutputDirectory();
    if (dir) setChosenDir(dir);
  }, []);

  const handleRunRename = useCallback(async () => {
    if (busy || actionableCount === 0) return;

    if (outputMode === "copy_to_chosen" && !chosenDir) {
      addLog(t("msg_rename_no_chosen_dir"), "error");
      return;
    }

    // Derive output paths up front so all confirmation dialogs see
    // the final names. Catch derive errors per-row — a bad row gets
    // logged + skipped, the rest of the batch proceeds.
    const derivedTargets: { row: PairingRow; outputPath: string }[] = [];
    for (const row of actionableRows) {
      try {
        const outputPath = deriveRenameOutputPath(
          row.video!.path,
          row.subtitle!.path,
          outputMode,
          chosenDir
        );
        derivedTargets.push({ row, outputPath });
      } catch (e) {
        const reason = e instanceof Error ? e.message : String(e);
        addLog(t("msg_rename_skipped", row.subtitle!.name, reason), "error");
      }
    }
    if (derivedTargets.length === 0) {
      addLog(t("msg_rename_nothing_to_do"), "error");
      return;
    }

    // No-op pre-flight. When a sub is already correctly named for its
    // paired video (e.g., DBD-Raws external-sub releases ship subs
    // matching `<videoBase>.<lang>.ass`), the derived output equals the
    // source path. Filtering these out BEFORE the overwrite dialog
    // avoids a spurious "N files already exist, overwrite?" prompt
    // followed by copyFile(src, src) failures. The remaining work goes
    // through the regular flow.
    const noopTargets: { row: PairingRow; outputPath: string }[] = [];
    const targets: { row: PairingRow; outputPath: string }[] = [];
    for (const tgt of derivedTargets) {
      if (isNoOpRename(tgt.row.subtitle!.path, tgt.outputPath)) {
        noopTargets.push(tgt);
      } else {
        targets.push(tgt);
      }
    }
    for (const tgt of noopTargets) {
      addLog(t("msg_rename_already_named", tgt.row.subtitle!.name), "info");
    }
    if (targets.length === 0) {
      // Nothing was actually written. Logging this as success + green
      // footer would suggest the rename worked; route through "noop"
      // (amber pending) so the user sees that the run made no changes.
      addLog(t("msg_rename_all_already_named", noopTargets.length), "info");
      setLastActionResult("noop");
      return;
    }

    // In-place rename is destructive (source disappears). Show a
    // confirmation dialog with the first 3 sample names so the user
    // sees exactly what will happen before committing.
    if (outputMode === "rename") {
      const samples = targets
        .slice(0, 3)
        .map((t2) => `${t2.row.subtitle!.name} → ${fileNameFromPath(t2.outputPath)}`)
        .join("\n");
      const moreCount = targets.length - 3;
      const moreSuffix = moreCount > 0 ? "\n" + t("msg_rename_inplace_more", moreCount) : "";
      const confirmed = await ask(
        t("msg_rename_inplace_confirm", targets.length) + "\n\n" + samples + moreSuffix,
        { title: t("dialog_rename_inplace_title"), kind: "warning" }
      );
      if (!confirmed) {
        addLog(t("msg_rename_cancelled"), "info");
        setLastActionResult("cancelled");
        return;
      }
    }

    // Pre-flight overwrite check — same project-wide pattern as the
    // other batch tabs. ANY existing target → single ask() with the
    // count; cancel preserves prior state, confirm proceeds. No-op
    // targets were filtered above so they don't inflate the count.
    const projectedOutputs = targets.map((t2) => t2.outputPath);
    const existingCount = await countExistingFiles(projectedOutputs);
    if (existingCount > 0) {
      const confirmed = await ask(t("msg_overwrite_confirm", existingCount, targets.length), {
        title: t("dialog_overwrite_title"),
        kind: "warning",
      });
      if (!confirmed) {
        addLog(t("msg_rename_cancelled"), "info");
        setLastActionResult("cancelled");
        return;
      }
    }

    setBusy(true);
    setProgress({ processed: 0, total: targets.length });
    cancelRef.current = false;

    try {
      addLog(t("msg_rename_start", targets.length, t(`rename_mode_${outputMode}_short`)));

      let successCount = 0;
      let processedCount = 0;
      const seenOutputs = new Set<string>();

      for (let i = 0; i < targets.length; i++) {
        if (cancelRef.current) {
          addLog(t("msg_rename_cancelled"), "info");
          break;
        }

        const { row, outputPath } = targets[i];
        const subName = row.subtitle!.name;
        const outName = fileNameFromPath(outputPath);
        addLog(t("msg_processing", subName));

        try {
          // Within-batch dedup. Two rows producing the same output
          // path (e.g., user pre-edited filenames in a way that
          // collides) would otherwise overwrite each other. No-op
          // rows (target == source) were filtered pre-flight.
          const normalizedOut = outputPath.normalize("NFC").replace(/\\/g, "/").toLowerCase();
          if (seenOutputs.has(normalizedOut)) {
            addLog(t("msg_skipped_duplicate", subName), "error");
            continue;
          }
          seenOutputs.add(normalizedOut);

          if (cancelRef.current) break;

          if (outputMode === "rename") {
            await renamePath(row.subtitle!.path, outputPath);
          } else {
            await copyPath(row.subtitle!.path, outputPath);
          }
          addLog(t("msg_rename_done", subName, outName), "success");
          successCount++;
        } catch (e) {
          const reason = e instanceof Error ? e.message : String(e);
          addLog(t("msg_rename_error", subName, reason), "error");
        } finally {
          processedCount++;
          setProgress({ processed: processedCount, total: targets.length });
        }
      }

      if (!cancelRef.current) {
        addLog(t("msg_rename_complete", successCount, targets.length), "success");
      }

      if (cancelRef.current) {
        setLastActionResult("cancelled");
      } else {
        setLastActionResult(successCount > 0 ? "success" : "error");
      }
    } finally {
      setBusy(false);
      setProgress(null);
    }
  }, [busy, actionableCount, actionableRows, outputMode, chosenDir, addLog, t]);

  // Reset last-action on selection change.
  useEffect(() => {
    setLastActionResult(null);
  }, [renameFiles]);

  const tabStatus = useMemo<Status>(() => {
    if (totalCount === 0) return { kind: "idle", message: t("status_rename_idle") };
    if (busy) {
      return {
        kind: "busy",
        message: t("status_rename_busy"),
        progress: progress ?? undefined,
      };
    }
    if (lastActionResult === "success") return { kind: "done", message: t("status_rename_done") };
    if (lastActionResult === "error") return { kind: "error", message: t("status_rename_error") };
    if (lastActionResult === "cancelled") {
      return { kind: "pending", message: t("status_rename_cancelled") };
    }
    if (lastActionResult === "noop") {
      return { kind: "pending", message: t("status_rename_noop") };
    }
    return {
      kind: "pending",
      message: t("status_rename_pending", videoCount, subtitleCount),
    };
  }, [totalCount, busy, progress, lastActionResult, videoCount, subtitleCount, t]);
  useTabStatus("rename", tabStatus);

  return (
    <div className="space-y-4">
      {/* File strip / drop zone — the only ingest surface in Stage 5a.
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
              background: "var(--cancel-bg)",
              color: "var(--cancel-text)",
              opacity: busy ? 0.4 : 1,
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
            color: busy ? "var(--text-muted)" : "white",
            height: "38px",
          }}
        >
          {t("btn_select_rename_inputs")}
        </button>
        {busy && (
          <button
            onClick={() => {
              cancelRef.current = true;
            }}
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
          onClick={handleRunRename}
          disabled={busy || actionableCount === 0}
          className="flex-none px-6 rounded-lg font-medium text-sm transition-colors"
          style={
            busy || actionableCount === 0
              ? {
                  background: "var(--accent-disabled-bg)",
                  color: "var(--accent-disabled-text)",
                  opacity: actionableCount === 0 ? 0.5 : 1,
                  height: "38px",
                  minWidth: "140px",
                }
              : { background: "var(--accent)", color: "#fff", height: "38px", minWidth: "140px" }
          }
        >
          {busy ? t("btn_renaming") : t("btn_rename_run", actionableCount)}
        </button>
      </div>

      {dropError && (
        <div
          className="flex items-center justify-between gap-3 px-3 py-2 rounded-lg text-sm"
          role="alert"
          style={{
            background: "var(--cancel-bg)",
            border: "1px solid var(--error)",
            color: "var(--error)",
          }}
        >
          <span>{dropError}</span>
          <button
            type="button"
            onClick={() => setDropError(null)}
            aria-label={t("btn_clear_file")}
            className="flex-none text-base"
            style={{ color: "var(--error)", lineHeight: 1 }}
          >
            ✕
          </button>
        </div>
      )}

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

      {/* Output-mode strategy. Three modes (per design doc 已决定 #3)
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
          <div className="flex flex-col gap-1.5">
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
                    color: busy ? "var(--text-muted)" : "white",
                  }}
                >
                  {t("btn_pick_chosen_dir")}
                </button>
                {chosenDir ? (
                  <span
                    className="text-xs truncate flex-1"
                    style={{ color: "var(--text-secondary)" }}
                    title={chosenDir}
                  >
                    {chosenDir}
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
      {logs.length > 0 && (
        <div
          className="rounded-lg"
          style={{ border: "1px solid var(--border)", background: "var(--bg-panel)" }}
        >
          <div
            className="flex items-center justify-between px-3 py-2"
            style={{ borderBottom: "1px solid var(--border)" }}
          >
            <span className="text-xs font-medium" style={{ color: "var(--text-muted)" }}>
              {t("log_title")}
            </span>
            <button
              onClick={() => setLogs([])}
              className="text-xs"
              style={{ color: "var(--text-muted)" }}
            >
              {t("log_clear")}
            </button>
          </div>
          <div
            ref={logScrollRef}
            className="max-h-48 overflow-y-auto p-3 font-mono text-xs space-y-0.5"
          >
            {logs.map((log) => (
              <div
                key={log.id}
                style={{
                  color: {
                    error: "var(--error)",
                    success: "var(--success)",
                    info: "var(--text-muted)",
                  }[log.type],
                }}
              >
                {log.text}
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

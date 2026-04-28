/**
 * Batch Rename — Tab 4.
 *
 * Stage 5a: tab plumbing + ingestion (file pick / folder drop, auto
 *   categorization, count chips, cross-tab dedup).
 * Stage 5b (this commit): pairing engine + preview grid. Each row
 *   represents one (video, subtitle) pair. Multi-language subs for the
 *   same video produce N rows with the first selected by default.
 * Stage 5c will add output-mode radios (rename / copy-to-video-dir /
 *   copy-to-chosen) + per-row checkbox toggles + manual edit + run.
 */
import { useState, useCallback, useEffect, useRef, useMemo } from "react";
import { pickRenameInputs, fileNameFromPath } from "../../lib/tauri-api";
import { useI18n } from "../../i18n/useI18n";
import { useFileContext } from "../../lib/FileContext";
import { TAB_LABEL_KEYS } from "../../lib/tab-labels";
import type { TabId } from "../../lib/FileContext";
import type { Status } from "../../lib/StatusContext";
import { useTabStatus } from "../../lib/useTabStatus";
import { useFolderDrop } from "../../lib/useFolderDrop";
import { PreviewTable, type PreviewTableColumn } from "../../lib/PreviewTable";
import {
  buildPairings,
  parseFilename,
  type PairingRow,
  type PairingSource,
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

type Category = "video" | "subtitle" | "unknown";

function categorize(name: string): Category {
  const dot = name.lastIndexOf(".");
  if (dot < 0) return "unknown";
  const ext = name.slice(dot + 1).toLowerCase();
  if (VIDEO_EXTS.has(ext)) return "video";
  if (SUBTITLE_EXTS.has(ext)) return "subtitle";
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

  // Stage 5a: rename hasn't been triggered yet, so `busy` is always
  // false and `progress` is always null. Both come back as full state
  // in Stage 5c when Save All lands. Keeping them as inert constants
  // here lets tabStatus + the strip's disabled rules read the same
  // shape they will once 5c lights up.
  const busy = false;
  const progress: { processed: number; total: number } | null = null;
  const [lastActionResult, setLastActionResult] = useState<
    "success" | "error" | "cancelled" | null
  >(null);
  const [dropActive, setDropActive] = useState(false);
  const [dropError, setDropError] = useState<string | null>(null);
  // Unknown-extension counter — surfaced in the chips but not in
  // renameFiles state, since the unknown bucket isn't pairing material.
  // Reset whenever renameFiles changes.
  const [unknownCount, setUnknownCount] = useState(0);
  const [logs, setLogs] = useState<LogEntry[]>([]);

  const pickGenRef = useRef(0);
  const logIdRef = useRef(0);
  const dropZoneRef = useRef<HTMLDivElement>(null);
  const logScrollRef = useRef<HTMLDivElement>(null);

  const videoPaths = useMemo(() => renameFiles?.videoPaths ?? [], [renameFiles]);
  const videoNames = useMemo(() => renameFiles?.videoNames ?? [], [renameFiles]);
  const subtitlePaths = useMemo(() => renameFiles?.subtitlePaths ?? [], [renameFiles]);
  const subtitleNames = useMemo(() => renameFiles?.subtitleNames ?? [], [renameFiles]);
  const videoCount = videoPaths.length;
  const subtitleCount = subtitlePaths.length;
  const totalCount = videoCount + subtitleCount;

  // Pairing — recomputed whenever the file lists change. Pure function
  // so memoizing on the array references is enough; the engine has no
  // hidden state.
  const pairingRows = useMemo<PairingRow[]>(() => {
    if (totalCount === 0) return [];
    const parsedVideos = videoPaths.map((p, i) => parseFilename(p, videoNames[i] ?? ""));
    const parsedSubs = subtitlePaths.map((p, i) => parseFilename(p, subtitleNames[i] ?? ""));
    return buildPairings(parsedVideos, parsedSubs);
  }, [videoPaths, videoNames, subtitlePaths, subtitleNames, totalCount]);

  const warningCount = useMemo(
    () => pairingRows.filter((r) => r.source === "warning").length,
    [pairingRows]
  );

  const pairingColumns = useMemo<PreviewTableColumn<PairingRow>[]>(
    () => [
      {
        key: "idx",
        header: t("col_index"),
        width: "32px",
        render: (_row, i) => i + 1,
        className: "row-idx",
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
        render: (row) =>
          row.subtitle ? (
            <span title={row.subtitle.name}>{row.subtitle.name}</span>
          ) : (
            <span style={{ color: "var(--text-muted)", fontStyle: "italic" }}>—</span>
          ),
      },
      {
        key: "source",
        header: t("rename_col_source"),
        width: "100px",
        render: (row) => renderSourceBadge(row.source, t),
      },
    ],
    [t]
  );

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

      {/* Pairing preview grid. The engine produces one row per
           (video, subtitle) pair; multi-language subs for the same
           video collapse to N rows. Source badge tells the user which
           algorithm decided the pair (regex / LCS / manual / unmatched
           / warning for ambiguous keys). Stage 5c will add per-row
           checkbox toggle + manual edit. */}
      {totalCount > 0 && (
        <PreviewTable
          rows={pairingRows}
          rowKey={(row) => row.id}
          columns={pairingColumns}
          title={
            <>
              <span>{t("rename_grid_title", pairingRows.length)}</span>
              {warningCount > 0 && (
                <span style={{ color: "var(--warning)" }}>
                  {" · "}
                  {t("rename_grid_warning_suffix", warningCount)}
                </span>
              )}
            </>
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

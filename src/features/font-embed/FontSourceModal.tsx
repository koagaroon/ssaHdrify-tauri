import { useCallback, useEffect, useRef, useState } from "react";
import {
  pickFontDirectory,
  pickFontFiles,
  scanFontDirectory,
  scanFontFiles,
  type LocalFontEntry,
} from "../../lib/tauri-api";
import { useI18n } from "../../i18n/useI18n";
import type { FontUsage } from "./font-collector";
import { fontKeyLabel } from "./font-collector";
import { userFontKey } from "./font-embedder";

export interface FontSource {
  /** Stable id used as a React key and for removal. */
  id: string;
  /** "dir" = picked a folder, "files" = picked individual files. */
  kind: "dir" | "files";
  /** Display label: folder basename or "N files". */
  label: string;
  /** Font entries this source contributed. */
  entries: LocalFontEntry[];
}

/** Diagnostic the parent returns after attempting to add a source. */
export interface AddSourceResult {
  /** How many entries made it into the source list (after dedup). */
  added: number;
  /** How many entries were filtered out because they were already loaded. */
  duplicated: number;
}

interface Props {
  open: boolean;
  onClose: () => void;
  sources: FontSource[];
  usages: FontUsage[];
  userFontMap: Map<string, LocalFontEntry>;
  hasSubtitle: boolean;
  onAddSource: (source: FontSource) => AddSourceResult;
  onRemoveSource: (id: string) => void;
}

function basename(path: string): string {
  return path.replace(/\\/g, "/").split("/").filter(Boolean).pop() ?? path;
}

function newId(): string {
  return `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
}

/** Compute how many required font families are matched by the user-supplied map. */
function computeCoverage(
  usages: FontUsage[],
  userFontMap: Map<string, LocalFontEntry>,
  hasSubtitle: boolean
): { covered: number; total: number; missing: string[] } {
  if (!hasSubtitle || usages.length === 0) {
    return { covered: 0, total: 0, missing: [] };
  }
  let covered = 0;
  const missing: string[] = [];
  for (const u of usages) {
    const k = userFontKey(u.key.family, u.key.bold, u.key.italic);
    if (userFontMap.has(k)) {
      covered += 1;
    } else {
      missing.push(fontKeyLabel(u.key));
    }
  }
  return { covered, total: usages.length, missing };
}

export default function FontSourceModal(props: Props) {
  const { open, onClose, sources, usages, userFontMap, hasSubtitle, onAddSource, onRemoveSource } =
    props;
  const { t } = useI18n();

  const [scanning, setScanning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // info is non-error feedback ("added N fonts") shown in a neutral tone.
  const [info, setInfo] = useState<string | null>(null);
  const panelRef = useRef<HTMLDivElement>(null);

  // Close on Escape.
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [open, onClose]);

  // Reset transient messages whenever the modal reopens.
  useEffect(() => {
    if (open) {
      setError(null);
      setInfo(null);
    }
  }, [open]);

  // Apply the dedup result consistently across folder and file picks.
  const applyAddResult = useCallback(
    (result: AddSourceResult) => {
      if (result.added === 0 && result.duplicated > 0) {
        setError(t("font_sources_all_duplicate"));
        setInfo(null);
      } else if (result.duplicated > 0) {
        setError(null);
        setInfo(t("font_sources_partial_duplicate", result.added, result.duplicated));
      } else {
        setError(null);
        setInfo(t("font_sources_added", result.added));
      }
    },
    [t]
  );

  const handleAddFolder = useCallback(async () => {
    setError(null);
    setInfo(null);
    const dir = await pickFontDirectory();
    if (!dir) return;
    setScanning(true);
    try {
      const entries = await scanFontDirectory(dir);
      if (entries.length === 0) {
        setError(t("font_sources_no_fonts_in_folder", basename(dir)));
        return;
      }
      const result = onAddSource({
        id: newId(),
        kind: "dir",
        label: basename(dir),
        entries,
      });
      applyAddResult(result);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setScanning(false);
    }
  }, [onAddSource, t, applyAddResult]);

  const handleAddFiles = useCallback(async () => {
    setError(null);
    setInfo(null);
    const paths = await pickFontFiles();
    if (!paths || paths.length === 0) return;
    setScanning(true);
    try {
      const entries = await scanFontFiles(paths);
      if (entries.length === 0) {
        setError(t("font_sources_no_fonts_in_files", paths.length));
        return;
      }
      const result = onAddSource({
        id: newId(),
        kind: "files",
        label: t("font_sources_files_entry", paths.length, entries.length),
        entries,
      });
      applyAddResult(result);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setScanning(false);
    }
  }, [onAddSource, t, applyAddResult]);

  // Coverage: how many required families are matched by ANY source
  // (local map OR — informationally — any other means). In this modal we
  // only consider the local map, so the count reflects the user's question:
  // "does the folder I picked cover every font the ASS needs?" System-
  // installed matches are shown as secondary info in the main font list.
  const { covered, total, missing } = computeCoverage(usages, userFontMap, hasSubtitle);

  if (!open) return null;

  const coverageComplete = total > 0 && covered === total;

  return (
    <div
      className="fixed inset-0 flex items-center justify-center z-50"
      style={{ background: "rgba(0,0,0,0.45)" }}
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        ref={panelRef}
        className="rounded-xl shadow-2xl w-full max-w-lg mx-4"
        style={{
          background: "var(--bg-app)",
          border: "1px solid var(--border)",
        }}
      >
        {/* ── Header ─────────────────────────────── */}
        <div
          className="px-5 py-3 flex items-center justify-between"
          style={{ borderBottom: "1px solid var(--border)" }}
        >
          <h2 className="text-sm font-semibold" style={{ color: "var(--text-primary)" }}>
            {t("font_sources_title")}
          </h2>
          <button
            onClick={onClose}
            className="px-2 py-1 rounded text-sm"
            style={{ color: "var(--text-muted)" }}
            title={t("font_sources_close")}
          >
            ✕
          </button>
        </div>

        {/* ── Body ───────────────────────────────── */}
        <div className="px-5 py-4 space-y-4">
          {/* Source list */}
          {sources.length === 0 ? (
            <p className="text-xs" style={{ color: "var(--text-muted)" }}>
              {t("font_sources_empty_hint")}
            </p>
          ) : (
            <ul
              className="rounded-lg overflow-hidden"
              style={{ border: "1px solid var(--border)" }}
            >
              {sources.map((src) => {
                const label =
                  src.kind === "dir"
                    ? t("font_sources_folder_entry", src.label, src.entries.length)
                    : src.label;
                return (
                  <li
                    key={src.id}
                    className="flex items-center justify-between px-3 py-2 text-sm"
                    style={{
                      borderBottom: "1px solid color-mix(in srgb, var(--border) 50%, transparent)",
                      color: "var(--text-primary)",
                    }}
                  >
                    <span className="truncate mr-3">{label}</span>
                    <button
                      onClick={() => onRemoveSource(src.id)}
                      className="px-2 py-0.5 rounded text-xs"
                      style={{
                        background: "var(--cancel-bg)",
                        color: "var(--cancel-text)",
                      }}
                      title={t("font_sources_remove")}
                    >
                      ✕
                    </button>
                  </li>
                );
              })}
            </ul>
          )}

          {/* Action buttons — both use the accent color because both are
              equally valid entry points; only the transient "scanning" state
              dims them. */}
          <div className="flex gap-2">
            <button
              onClick={handleAddFolder}
              disabled={scanning}
              className="flex-1 px-4 py-2 rounded-lg font-medium text-sm transition-colors"
              style={
                scanning
                  ? {
                      background: "var(--accent-disabled-bg)",
                      color: "var(--accent-disabled-text)",
                      opacity: 0.6,
                    }
                  : { background: "var(--accent)", color: "white" }
              }
            >
              {scanning ? t("font_sources_scanning") : `+ ${t("font_sources_add_folder")}`}
            </button>
            <button
              onClick={handleAddFiles}
              disabled={scanning}
              className="flex-1 px-4 py-2 rounded-lg font-medium text-sm transition-colors"
              style={
                scanning
                  ? {
                      background: "var(--accent-disabled-bg)",
                      color: "var(--accent-disabled-text)",
                      opacity: 0.6,
                    }
                  : { background: "var(--accent)", color: "white" }
              }
            >
              + {t("font_sources_add_files")}
            </button>
          </div>

          {error && (
            <p className="text-xs" style={{ color: "var(--error)" }}>
              {error}
            </p>
          )}

          {info && !error && (
            <p className="text-xs" style={{ color: "var(--success)" }}>
              {info}
            </p>
          )}

          {/* Coverage panel */}
          <div
            className="rounded-lg px-3 py-3"
            style={{
              border: "1px solid var(--border)",
              background: "var(--bg-panel)",
            }}
          >
            {!hasSubtitle ? (
              <p className="text-xs" style={{ color: "var(--text-muted)" }}>
                {t("font_coverage_no_subtitle")}
              </p>
            ) : (
              <div className="space-y-2">
                <p
                  className="text-sm font-medium"
                  style={{
                    color: coverageComplete ? "var(--badge-green-text)" : "var(--text-primary)",
                  }}
                >
                  {t("font_coverage", covered, total)}
                  {coverageComplete && (
                    <span
                      className="ml-2 text-xs px-2 py-0.5 rounded"
                      style={{
                        background: "var(--badge-green-bg)",
                        color: "var(--badge-green-text)",
                      }}
                    >
                      {t("font_coverage_complete")}
                    </span>
                  )}
                </p>
                {missing.length > 0 && (
                  <p className="text-xs" style={{ color: "var(--text-muted)" }}>
                    {t("font_coverage_missing", missing.join(", "))}
                  </p>
                )}
              </div>
            )}
          </div>
        </div>

        {/* ── Footer ─────────────────────────────── */}
        <div
          className="px-5 py-3 flex justify-end"
          style={{ borderTop: "1px solid var(--border)" }}
        >
          <button
            onClick={onClose}
            className="px-4 py-2 rounded-lg text-sm"
            style={{
              background: "var(--bg-input)",
              color: "var(--text-primary)",
              border: "1px solid var(--border)",
            }}
          >
            {t("font_sources_close")}
          </button>
        </div>
      </div>
    </div>
  );
}

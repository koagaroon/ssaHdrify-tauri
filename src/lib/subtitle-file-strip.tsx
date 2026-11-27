import type { JSX, ReactNode, RefObject } from "react";

export interface SubtitleFileStripProps {
  dropZoneRef: RefObject<HTMLDivElement | null>;
  fileContainerRef: RefObject<HTMLDivElement | null>;
  dropActive: boolean;
  filePaths: readonly string[];
  fileNames: readonly string[];
  showFileList: boolean;
  onToggleFileList: () => void;
  emptyLabel: string;
  selectedTitle: string;
  clearTitle: string;
  selectLabel: string;
  clearDisabled: boolean;
  selectDisabled: boolean;
  onClearFiles: () => void;
  onSelectFiles: () => void;
  /** Optional inline metadata rendered inside the multi-file summary button. */
  multiFileBadge?: ReactNode;
  /** Optional inline metadata rendered after the single-file name. */
  singleFileMeta?: ReactNode;
  /** Optional trailing controls (e.g. Cancel + Convert/Save/Write buttons). */
  trailingActions?: ReactNode;
}

/**
 * Pure JSX builder for the shared subtitle file-selection strip across
 * HDR Convert, Time Shift, Font Embed, and Style Edit.
 *
 * Implemented as a hook-free JSX helper so direct function-call test harnesses
 * (see `batch-write-handlers.test.tsx`) inspect its action buttons inline
 * without shifting React hook slots.
 */
export function renderSubtitleFileStrip({
  dropZoneRef,
  fileContainerRef,
  dropActive,
  filePaths,
  fileNames,
  showFileList,
  onToggleFileList,
  emptyLabel,
  selectedTitle,
  clearTitle,
  selectLabel,
  clearDisabled,
  selectDisabled,
  onClearFiles,
  onSelectFiles,
  multiFileBadge,
  singleFileMeta,
  trailingActions,
}: SubtitleFileStripProps): JSX.Element {
  const fileCount = filePaths.length;
  return (
    <div
      ref={dropZoneRef}
      className={`drop-zone flex items-center gap-2${dropActive ? " drop-active" : ""}`}
    >
      <div ref={fileContainerRef} className="flex-1 min-w-0" style={{ position: "relative" }}>
        {fileCount > 1 ? (
          <button
            type="button"
            onClick={onToggleFileList}
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
            {multiFileBadge}
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
                  {fileNames[0]}
                </span>
                {singleFileMeta}
              </>
            ) : (
              <span className="italic" style={{ color: "var(--text-muted)" }}>
                {emptyLabel}
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
            aria-label={selectedTitle}
          >
            <div
              className="px-3 py-2 flex-none"
              style={{ borderBottom: "1px solid var(--border)" }}
            >
              <span className="text-xs font-medium" style={{ color: "var(--text-muted)" }}>
                {selectedTitle}
              </span>
            </div>
            <div className="overflow-y-auto flex-1">
              {fileNames.map((name, idx) => (
                <div
                  key={filePaths[idx] ?? idx}
                  role="option"
                  aria-selected={true}
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
          type="button"
          onClick={onClearFiles}
          disabled={clearDisabled}
          className="flex-none px-3 rounded-lg text-lg font-bold transition-colors"
          style={{
            background: clearDisabled ? "var(--bg-input)" : "var(--cancel-bg)",
            color: clearDisabled ? "var(--text-muted)" : "var(--cancel-text)",
            height: "38px",
          }}
          title={clearTitle}
          aria-label={clearTitle}
        >
          ✕
        </button>
      )}
      <button
        type="button"
        onClick={onSelectFiles}
        disabled={selectDisabled}
        className="flex-none px-5 rounded-lg font-medium text-sm transition-colors"
        style={{
          background: selectDisabled ? "var(--bg-input)" : "var(--accent)",
          color: selectDisabled ? "var(--text-muted)" : "var(--accent-text)",
          height: "38px",
        }}
      >
        {selectLabel}
      </button>
      {trailingActions}
    </div>
  );
}

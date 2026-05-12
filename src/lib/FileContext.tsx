/**
 * Shared file-state context across all tabs.
 *
 * DESIGN DECISION — silent replace on re-select:
 * When the user clicks "Select File" in a tab that already has a file loaded,
 * the new selection silently replaces the old one WITHOUT a confirmation prompt.
 * Rationale: the user explicitly opened a native file picker and chose a new
 * file — that is a clear intent to switch. This matches standard behavior in
 * text editors, image viewers, and similar tools. The big × (clear) button
 * covers the "start over" case. Re-selection is NOT a destructive action
 * because no processing has been triggered yet (Select and Convert/Save are
 * separate buttons). Do NOT add a confirmation dialog for re-selection —
 * it would interrupt a natural workflow for no safety benefit.
 *
 * Cross-tab duplicate guard:
 * A given file path can only be loaded in ONE tab at a time. This prevents
 * accidental overwrites when different tabs produce output from the same
 * source file. If the user needs the file in a different tab, they must
 * clear it from the current tab first (via the × button).
 */

import { createContext, useCallback, useContext, useMemo, useState, type ReactNode } from "react";

import { pathsEqualOnFs } from "./path-validation";

// ── Types ────────────────────────────────────────────────

export type TabId = "hdr" | "timing" | "fonts" | "rename";

export interface HdrFileState {
  filePaths: string[];
  fileNames: string[];
}

export interface TimingFilesState {
  filePaths: string[];
  fileNames: string[];
  /** Content of `filePaths[0]` only, cached for the live timeline preview.
   *  In a batch of N>1 files we don't pre-load all bodies — they're read
   *  during the save loop. The preview is a sample of what the offset
   *  does to the first file; the same offset applies uniformly to the
   *  rest, so single-file preview is honest for batch too. */
  firstFileContent: string;
}

export interface FontsFilesState {
  filePaths: string[];
  fileNames: string[];
  /** Content of `filePaths[0]` only — used in single-file mode for the
   *  detection grid + per-font selection. In batch (length > 1) the grid
   *  is hidden and remaining files are analyzed during the embed loop. */
  firstFileContent: string;
}

/** Tab 4 (Batch Rename) holds two parallel arrays — videos and
 *  subtitles. Cross-tab dedup only treats SUBTITLES as conflict sources
 *  (other tabs don't care about videos), but within Tab 4 we track both
 *  for the pairing UI. */
export interface BatchRenameFilesState {
  videoPaths: string[];
  videoNames: string[];
  subtitlePaths: string[];
  subtitleNames: string[];
}

interface FileContextValue {
  hdrFiles: HdrFileState | null;
  timingFiles: TimingFilesState | null;
  fontsFiles: FontsFilesState | null;
  renameFiles: BatchRenameFilesState | null;

  setHdrFiles: (state: HdrFileState | null) => void;
  setTimingFiles: (state: TimingFilesState | null) => void;
  setFontsFiles: (state: FontsFilesState | null) => void;
  setRenameFiles: (state: BatchRenameFilesState | null) => void;
  clearFile: (tab: TabId) => void;

  /**
   * Check whether a file path is already loaded in any tab.
   * Returns the tab ID if in use, or null if free.
   */
  isFileInUse: (path: string, excludeTab?: TabId) => TabId | null;
}

const FileContext = createContext<FileContextValue | null>(null);

// ── Provider ─────────────────────────────────────────────

export function FileProvider({ children }: { children: ReactNode }) {
  const [hdrFiles, setHdrFiles] = useState<HdrFileState | null>(null);
  const [timingFiles, setTimingFiles] = useState<TimingFilesState | null>(null);
  const [fontsFiles, setFontsFiles] = useState<FontsFilesState | null>(null);
  const [renameFiles, setRenameFiles] = useState<BatchRenameFilesState | null>(null);

  const isFileInUse = useCallback(
    (path: string, excludeTab?: TabId): TabId | null => {
      // `pathsEqualOnFs` handles separator + case folding conditionally
      // on the runtime FS (`\` is a valid filename char on POSIX; only
      // NTFS / APFS / HFS+ are case-insensitive). Without that gating
      // Linux users would see false-positive collisions between
      // legitimately distinct files like `Episode.ass` vs `episode.ass`.
      if (excludeTab !== "hdr" && hdrFiles?.filePaths.some((p) => pathsEqualOnFs(p, path))) {
        return "hdr";
      }
      if (
        excludeTab !== "timing" &&
        timingFiles?.filePaths.some((p) => pathsEqualOnFs(p, path))
      ) {
        return "timing";
      }
      if (excludeTab !== "fonts" && fontsFiles?.filePaths.some((p) => pathsEqualOnFs(p, path))) {
        return "fonts";
      }
      // Tab 4 holds both videos and subtitles, but only the SUBTITLE
      // entries are subject to cross-tab dedup with other tabs (HDR /
      // Timing / Fonts all consume subtitles). Videos in Tab 4 don't
      // conflict with anything in the other three tabs. Within Tab 4
      // itself, both lists are checked (excludeTab === "rename" skips
      // this whole block).
      if (
        excludeTab !== "rename" &&
        renameFiles?.subtitlePaths.some((p) => pathsEqualOnFs(p, path))
      ) {
        return "rename";
      }
      return null;
    },
    [hdrFiles, timingFiles, fontsFiles, renameFiles]
  );

  const clearFile = useCallback((tab: TabId) => {
    switch (tab) {
      case "hdr":
        setHdrFiles(null);
        break;
      case "timing":
        setTimingFiles(null);
        break;
      case "fonts":
        setFontsFiles(null);
        break;
      case "rename":
        setRenameFiles(null);
        break;
    }
  }, []);

  // Memoize so consumers reading `useFileContext()` don't re-render on every
  // parent re-render. The value identity changes only when a state slot or
  // stable callback reference actually changes.
  const value = useMemo(
    () => ({
      hdrFiles,
      timingFiles,
      fontsFiles,
      renameFiles,
      setHdrFiles,
      setTimingFiles,
      setFontsFiles,
      setRenameFiles,
      clearFile,
      isFileInUse,
    }),
    [hdrFiles, timingFiles, fontsFiles, renameFiles, clearFile, isFileInUse]
  );

  return <FileContext.Provider value={value}>{children}</FileContext.Provider>;
}

// ── Hook ─────────────────────────────────────────────────

// eslint-disable-next-line react-refresh/only-export-components -- co-located Provider + hook is standard React pattern
export function useFileContext(): FileContextValue {
  const ctx = useContext(FileContext);
  if (!ctx) {
    throw new Error("useFileContext must be used within a FileProvider");
  }
  return ctx;
}

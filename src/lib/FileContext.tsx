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

// ── Types ────────────────────────────────────────────────

export type TabId = "hdr" | "timing" | "fonts";

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

interface FileContextValue {
  hdrFiles: HdrFileState | null;
  timingFiles: TimingFilesState | null;
  fontsFiles: FontsFilesState | null;

  setHdrFiles: (state: HdrFileState | null) => void;
  setTimingFiles: (state: TimingFilesState | null) => void;
  setFontsFiles: (state: FontsFilesState | null) => void;
  clearFile: (tab: TabId) => void;

  /**
   * Check whether a file path is already loaded in any tab.
   * Returns the tab ID if in use, or null if free.
   */
  isFileInUse: (path: string, excludeTab?: TabId) => TabId | null;

  /**
   * Filter an array of paths, returning only those NOT already loaded
   * in other tabs. Used by HDR multi-select to skip duplicates.
   * Returns { allowed: string[], skippedCount: number }.
   */
  filterAvailablePaths: (
    paths: string[],
    currentTab: TabId
  ) => { allowed: string[]; skippedCount: number };
}

const FileContext = createContext<FileContextValue | null>(null);

// ── Provider ─────────────────────────────────────────────

export function FileProvider({ children }: { children: ReactNode }) {
  const [hdrFiles, setHdrFiles] = useState<HdrFileState | null>(null);
  const [timingFiles, setTimingFiles] = useState<TimingFilesState | null>(null);
  const [fontsFiles, setFontsFiles] = useState<FontsFilesState | null>(null);

  const isFileInUse = useCallback(
    (path: string, excludeTab?: TabId): TabId | null => {
      // Normalize to forward-slash + lowercase for Windows case-insensitive comparison.
      // Without this, the same physical file at different cases (e.g., "Movie.ass"
      // vs "movie.ass") would bypass the duplicate guard on Windows.
      const norm = (p: string) => p.replace(/\\/g, "/").toLowerCase();
      const np = norm(path);
      if (excludeTab !== "hdr" && hdrFiles?.filePaths.some((p) => norm(p) === np)) {
        return "hdr";
      }
      if (excludeTab !== "timing" && timingFiles?.filePaths.some((p) => norm(p) === np)) {
        return "timing";
      }
      if (excludeTab !== "fonts" && fontsFiles?.filePaths.some((p) => norm(p) === np)) {
        return "fonts";
      }
      return null;
    },
    [hdrFiles, timingFiles, fontsFiles]
  );

  // INTENTIONALLY UNUSED — kept as a partial-skip alternative to the
  // strict-reject pattern in `isFileInUse` + per-tab `checkConflicts`.
  // Tabs currently reject the entire selection on any cross-tab conflict
  // (so the user sees a single visible banner naming the conflicting
  // tab) — but a future flow that wants "load what you can, skip the
  // rest" semantics (a Tab-4 batch-rename multi-source merge, for
  // example) can adopt this without re-deriving the loop logic. Do not
  // delete as dead code: the strict-vs-skip choice belongs to the
  // consumer, not the FileContext layer.
  const filterAvailablePaths = useCallback(
    (paths: string[], currentTab: TabId) => {
      const allowed: string[] = [];
      let skippedCount = 0;
      for (const p of paths) {
        if (isFileInUse(p, currentTab) !== null) {
          skippedCount += 1;
        } else {
          allowed.push(p);
        }
      }
      return { allowed, skippedCount };
    },
    [isFileInUse]
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
      setHdrFiles,
      setTimingFiles,
      setFontsFiles,
      clearFile,
      isFileInUse,
      filterAvailablePaths,
    }),
    [hdrFiles, timingFiles, fontsFiles, clearFile, isFileInUse, filterAvailablePaths]
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

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

import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useState,
  type ReactNode,
} from "react";

// ── Types ────────────────────────────────────────────────

export type TabId = "hdr" | "timing" | "fonts";

export interface HdrFileState {
  filePaths: string[];
  fileNames: string[];
}

export interface TimingFileState {
  filePath: string;
  fileName: string;
  fileContent: string;
}

export interface FontsFileState {
  filePath: string;
  fileName: string;
  fileContent: string;
}

interface FileContextValue {
  hdrFiles: HdrFileState | null;
  timingFile: TimingFileState | null;
  fontsFile: FontsFileState | null;

  setHdrFiles: (state: HdrFileState | null) => void;
  setTimingFile: (state: TimingFileState | null) => void;
  setFontsFile: (state: FontsFileState | null) => void;
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
  const [timingFile, setTimingFile] = useState<TimingFileState | null>(null);
  const [fontsFile, setFontsFile] = useState<FontsFileState | null>(null);

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
      if (excludeTab !== "timing" && timingFile && norm(timingFile.filePath) === np) {
        return "timing";
      }
      if (excludeTab !== "fonts" && fontsFile && norm(fontsFile.filePath) === np) {
        return "fonts";
      }
      return null;
    },
    [hdrFiles, timingFile, fontsFile]
  );

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
        setTimingFile(null);
        break;
      case "fonts":
        setFontsFile(null);
        break;
    }
  }, []);

  // Memoize so consumers reading `useFileContext()` don't re-render on every
  // parent re-render. The value identity changes only when a state slot or
  // stable callback reference actually changes.
  const value = useMemo(
    () => ({
      hdrFiles,
      timingFile,
      fontsFile,
      setHdrFiles,
      setTimingFile,
      setFontsFile,
      clearFile,
      isFileInUse,
      filterAvailablePaths,
    }),
    [hdrFiles, timingFile, fontsFile, clearFile, isFileInUse, filterAvailablePaths]
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

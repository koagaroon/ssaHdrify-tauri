/**
 * Cross-tab dedup helpers — extracted from the four feature tabs that
 * each implemented the same loop / normalization independently.
 *
 * `buildConflictMessage`: scan a list of paths, identify which ones are
 * already loaded in another tab, and return a localized "blocked"
 * message naming the conflicting tab and the count. Returns null when
 * the selection is safe to load. The strict-reject pattern (any
 * conflict rejects the WHOLE selection) is intentional — see
 * FileContext.tsx commentary.
 *
 * `normalizeOutputKey`: the dedup key shared by within-batch output
 * collision checks (HDR / Timing / Fonts / Rename loops). NFC + forward
 * slash + lowercase, so Windows case-insensitive paths and
 * macOS HFS+/APFS-produced NFD filenames don't appear distinct from
 * their NFC counterparts on disk.
 */
import type { TabId } from "./FileContext";
import { TAB_LABEL_KEYS } from "./tab-labels";

/** Translator signature used by `buildConflictMessage`. Matches the
 *  `t` callback returned from `useI18n`. */
type Translator = (key: string, ...args: (string | number)[]) => string;

/** Lookup signature — typically `useFileContext().isFileInUse`. */
type IsFileInUse = (path: string, excludeTab?: TabId) => TabId | null;

/**
 * Build a localized "this selection conflicts with another tab" message
 * by walking `paths` and asking `isFileInUse` for each. Returns null
 * when nothing conflicts (selection is safe to proceed).
 */
export function buildConflictMessage(
  paths: string[],
  currentTab: TabId,
  isFileInUse: IsFileInUse,
  t: Translator
): string | null {
  let conflictCount = 0;
  let conflictTab: TabId | null = null;
  for (const p of paths) {
    const usedIn = isFileInUse(p, currentTab);
    if (usedIn) {
      if (conflictTab === null) conflictTab = usedIn;
      conflictCount++;
    }
  }
  if (conflictTab === null) return null;
  return t("msg_dedup_blocked", conflictCount, t(TAB_LABEL_KEYS[conflictTab]));
}

/**
 * Canonical dedup key for an output path. NFC normalization + forward
 * slashes + lowercase so the same on-disk file isn't seen as two
 * distinct outputs across encodings or path-separator conventions.
 */
export function normalizeOutputKey(path: string): string {
  return path.normalize("NFC").replace(/\\/g, "/").toLowerCase();
}

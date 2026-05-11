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
 * slash + (case-insensitive-FS-only) lowercase, so the same on-disk
 * file isn't seen as two distinct outputs across encodings or path-
 * separator conventions on Windows / macOS while Linux ext4/btrfs/xfs
 * keep case-distinct names distinct.
 */
import type { TabId } from "./FileContext";
import { TAB_LABEL_KEYS } from "./tab-labels";
import { isCaseInsensitiveFs } from "./platform";

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
  // Track conflicts grouped by tab so a multi-tab collision (e.g.,
  // two paths in HDR + one in Time Shift) lists every blocking tab,
  // not just the first one we hit. Previous "first conflict only"
  // behavior misled users who saw "blocked by HDR Convert" while a
  // file was actually also in Time Shift.
  const conflictsByTab = new Map<TabId, number>();
  for (const p of paths) {
    const usedIn = isFileInUse(p, currentTab);
    if (usedIn) {
      conflictsByTab.set(usedIn, (conflictsByTab.get(usedIn) ?? 0) + 1);
    }
  }
  if (conflictsByTab.size === 0) return null;
  // Single-tab path keeps the existing message exactly so callers /
  // tests anchored on it stay green.
  if (conflictsByTab.size === 1) {
    const [tab, count] = conflictsByTab.entries().next().value!;
    return t("msg_dedup_blocked", count, t(TAB_LABEL_KEYS[tab]));
  }
  // Multi-tab: list each "{count} in {tab}" segment, joined by "/".
  // Total count is the sum so the leading "{N} blocked" claim still
  // matches the user's actual selection size.
  const totalCount = Array.from(conflictsByTab.values()).reduce((a, b) => a + b, 0);
  const tabs = Array.from(conflictsByTab.entries())
    .map(([tab, count]) => `${count} ${t(TAB_LABEL_KEYS[tab])}`)
    .join(" / ");
  return t("msg_dedup_blocked", totalCount, tabs);
}

/**
 * Canonical dedup key for an output path. NFC normalization + forward
 * slashes + (on case-insensitive filesystems) lowercase so the same
 * on-disk file isn't seen as two distinct outputs across encodings or
 * path-separator conventions.
 *
 * Lowercase is gated on `isCaseInsensitiveFs` so Linux ext4/btrfs/xfs
 * (case-sensitive) keeps `Episode.ass` and `episode.ass` as distinct
 * outputs while Windows NTFS / macOS APFS / HFS+ (case-insensitive by
 * default) collapses them — matches OS-level filesystem semantics so
 * the dedup catches real on-disk collisions but doesn't over-merge on
 * platforms where case-only names are legitimately distinct (Codex
 * dd2d9554).
 */
export function normalizeOutputKey(path: string): string {
  const normalized = path.normalize("NFC").replace(/\\/g, "/");
  return isCaseInsensitiveFs ? normalized.toLowerCase() : normalized;
}

/** Bidirectional controls + line/paragraph separators whose visual
 *  effect bypasses the "OS-native dialog renders text plainly"
 *  assumption. Listed individually so the intent is grep-able:
 *  - U+061C — Arabic Letter Mark (Trojan-Source vector per Unicode TR9)
 *  - U+200E / U+200F — LRM / RLM marks
 *  - U+202A..U+202E — LRE / RLE / PDF / LRO / RLO embeddings + overrides
 *    (CVE-2021-42574 core vector)
 *  - U+2028 / U+2029 — LINE SEPARATOR / PARAGRAPH SEPARATOR (honored as
 *    real line breaks by macOS NSAlert, which would split a single
 *    sample row across multiple visible lines)
 *  - U+2066..U+2069 — LRI / RLI / FSI / PDI isolates
 */
// Use \u escapes for every entry rather than literal characters:
// U+2028 LINE SEPARATOR is treated as a line terminator by the
// TypeScript regex parser, which would close the regex literal
// mid-pattern if it appeared in the source verbatim. \u escapes
// keep the parser happy and the intent grep-able.
const DIALOG_BIDI_CONTROLS_RE = /[\u061C\u200E\u200F\u202A-\u202E\u2028\u2029\u2066-\u2069]/g;

/** Strip bidirectional control characters and stray line separators
 *  before rendering an untrusted string inside a native `ask()` dialog
 *  body. Without this, a malicious filename containing U+202E
 *  (RIGHT-TO-LEFT OVERRIDE) can visually reverse the rename arrow +
 *  filename in the OS-native dialog and trick the user into confirming
 *  an unintended rename (CVE-2021-42574 class). Apply at any callsite
 *  that interpolates an untrusted name / path / error into an `ask()`
 *  body.
 *
 *  Audit (2026-05-04): the only `ask()` callsite in the codebase that
 *  interpolates untrusted text is the BatchRename in-place-rename
 *  sample row (BatchRename.tsx); every other site renders only counts
 *  / pre-formatted byte sizes / fully-translated literals. If a future
 *  callsite adds a filename, path, or backend error string into an
 *  `ask()` body, sanitize it here. Counts and other non-name strings
 *  are unaffected. */
export function sanitizeForDialog(name: string): string {
  return name.replace(DIALOG_BIDI_CONTROLS_RE, "");
}

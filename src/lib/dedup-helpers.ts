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
 * slashes + lowercase so the same on-disk file isn't seen as two
 * distinct outputs across encodings or path-separator conventions.
 */
export function normalizeOutputKey(path: string): string {
  return path.normalize("NFC").replace(/\\/g, "/").toLowerCase();
}

/** Bidirectional controls whose visual reordering effect is the
 *  Trojan-Source attack class (CVE-2021-42574). Listed individually so
 *  the intent is grep-able:
 *  - U+202A..U+202E — LRE / RLE / PDF / LRO / RLO embeddings + overrides
 *  - U+2066..U+2069 — LRI / RLI / FSI / PDI isolates
 *  - U+200E / U+200F — LRM / RLM marks (not strictly needed for the
 *    Trojan Source vector, but combined with the others they produce
 *    visual deception in plain-text renderers).
 */
const DIALOG_BIDI_CONTROLS_RE = /[‪-‮⁦-⁩‎‏]/g;

/** Strip bidirectional control characters before rendering a filename
 *  inside an `ask()` dialog body. Without this, a malicious subtitle
 *  filename containing U+202E (RIGHT-TO-LEFT OVERRIDE) can visually
 *  reverse the rename arrow + filename in the OS-native dialog and
 *  trick the user into confirming an unintended rename. Apply at any
 *  callsite that interpolates an untrusted filename into an `ask()`
 *  body — counts and other non-name strings are unaffected. */
export function sanitizeForDialog(name: string): string {
  return name.replace(DIALOG_BIDI_CONTROLS_RE, "");
}

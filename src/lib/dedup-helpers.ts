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
import { BIDI_AND_ZERO_WIDTH_GLOBAL_RE } from "./unicode-controls";
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
    // Defensive destructure (N-R5-FELIB-13): the `size === 1` guard
    // above proves the iterator yields one entry, so the old
    // `.next().value!` non-null assertion was sound — but a future
    // refactor that merges the size-1 branch into the multi-tab path
    // would silently produce `undefined` here. Array.from spread keeps
    // the destructure unambiguous and crashes loudly on the impossible
    // case instead of returning `t("msg_dedup_blocked", undefined, ...)`.
    const [tab, count] = Array.from(conflictsByTab.entries())[0];
    return t("msg_dedup_blocked", count, t(TAB_LABEL_KEYS[tab]));
  }
  // Multi-tab: list each "{count} in {tab}" segment, joined by "/".
  // Total count is the sum so the leading "{N} blocked" claim still
  // matches the user's actual selection size.
  //
  // Round 6 Wave 6.5 #19: uses `msg_dedup_blocked_multi` (no "in the
  // ... tab" suffix) so the compound "3 HDR / 2 Shift" reads
  // naturally instead of "... in the 3 HDR / 2 Shift tab".
  const totalCount = Array.from(conflictsByTab.values()).reduce((a, b) => a + b, 0);
  const tabs = Array.from(conflictsByTab.entries())
    .map(([tab, count]) => `${count} ${t(TAB_LABEL_KEYS[tab])}`)
    .join(" / ");
  return t("msg_dedup_blocked_multi", totalCount, tabs);
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

/** Strip BiDi controls, line/paragraph separators, AND zero-width
 *  characters before rendering an untrusted string inside a native
 *  `ask()` dialog body. Without this, a malicious filename containing
 *  U+202E (RIGHT-TO-LEFT OVERRIDE) can visually reverse the rename
 *  arrow + filename in the OS-native dialog and trick the user into
 *  confirming an unintended rename (CVE-2021-42574 class). Zero-width
 *  chars (U+200B-U+200D, U+FEFF) are also scrubbed: a filename like
 *  `EP<U+200B>01.ass` renders identically to `EP01.ass` while matching
 *  a different on-disk path. Codepoint enumeration is shared with the
 *  Rust-side `validate_font_family` / `validate_ipc_path` rejection
 *  sets via `unicode-controls.ts`.
 *
 *  Audit (2026-05-04): the only `ask()` callsite in the codebase that
 *  interpolates untrusted text is the BatchRename in-place-rename
 *  sample row (BatchRename.tsx); every other site renders only counts
 *  / pre-formatted byte sizes / fully-translated literals. If a future
 *  callsite adds a filename, path, or backend error string into an
 *  `ask()` body, sanitize it here. Counts and other non-name strings
 *  are unaffected. */
export function sanitizeForDialog(name: string): string {
  return name.replace(BIDI_AND_ZERO_WIDTH_GLOBAL_RE, "");
}

/** Round 7 Wave 7.1 — shared catch-arm helper. Normalizes the
 *  `e: unknown` thrown by IPC / async code to a string and scrubs
 *  BiDi / zero-width controls in one call. Adopted across all four
 *  tab handlers + FontCacheDriftModal so every render of an error
 *  message into the log panel / dialog body / status banner goes
 *  through the same sanitizer — eliminates the per-callsite "did
 *  I remember to wrap?" question. */
export function sanitizeError(e: unknown): string {
  return sanitizeForDialog(e instanceof Error ? e.message : String(e));
}

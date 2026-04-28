import type { TabId } from "./FileContext";

/**
 * Tab id → i18n key map. Used by callers that need to translate a tab
 * name at runtime (e.g., "this file is already in <TAB> tab" messages).
 * Explicit map instead of `"tab_" + id` concat so renaming a `TabId`
 * value fails at compile time rather than silently returning the bare
 * key. Lives in its own file so FileContext.tsx can continue to export
 * a Provider + hook together without tripping react-refresh's
 * "one component per file" rule.
 */
export const TAB_LABEL_KEYS: Record<TabId, string> = {
  hdr: "tab_hdr",
  timing: "tab_timing",
  fonts: "tab_fonts",
  rename: "tab_rename",
};

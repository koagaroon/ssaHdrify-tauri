import { normalizeOutputKey } from "../../lib/dedup-helpers";
import { parseFiniteNumberText } from "../../lib/strict-number";
import { ASCII_CONTROL_CHARS, hasUnicodeControls } from "../../lib/unicode-controls";

export const STYLE_EDIT_MAX_FILES = 500;
export const STYLE_EDIT_MAX_SOURCE_BYTES = 200 * 1024 * 1024;
export const STYLE_EDIT_MAX_DECODED_BYTES = 200 * 1024 * 1024;
export const STYLE_EDIT_MAX_ROWS = 2_000;
export const STYLE_EDIT_MIN_FONT_SIZE = 1;
export const STYLE_EDIT_MAX_FONT_SIZE = 200;

export function filterAndDedupeStyleEditPaths(paths: readonly string[]): string[] {
  const unique = new Map<string, string>();
  for (const path of paths) {
    const name = path.split(/[\\/]/).pop() ?? path;
    const dot = name.lastIndexOf(".");
    const extension = dot >= 0 ? name.slice(dot + 1).toLowerCase() : "";
    if (extension !== "ass" && extension !== "ssa") continue;
    const key = normalizeOutputKey(path);
    if (!unique.has(key)) unique.set(key, path);
  }
  return Array.from(unique.values());
}

export function reconcileStyleSelection(
  selected: ReadonlySet<string>,
  previousChangeable: ReadonlySet<string>,
  nextChangeable: ReadonlySet<string>
): Set<string> {
  const next = new Set<string>();
  for (const key of nextChangeable) {
    if (!previousChangeable.has(key) || selected.has(key)) next.add(key);
  }
  return next;
}

export type FontFamilyValidationError =
  "required" | "surrounding_whitespace" | "too_long" | "comma" | "control";

export interface StyleEditOperationState {
  fontFamilyEnabled: boolean;
  targetFontFamily: string;
  sourceFilterEnabled: boolean;
  sourceFontFamily: string;
  fontSizeEnabled: boolean;
  targetFontSize: string;
}

export interface StyleEditOperationValidation {
  targetFontFamily: string | null;
  sourceFontFamily: string | null;
  targetFontSize: number | null;
  targetFontError: FontFamilyValidationError | null;
  sourceFontError: FontFamilyValidationError | null;
  fontSizeInvalid: boolean;
  hasEnabledOperation: boolean;
  valid: boolean;
}

export function validateStyleFontFamily(value: string): FontFamilyValidationError | null {
  if (value.trim() === "") return "required";
  if (value !== value.trim()) return "surrounding_whitespace";
  if (Array.from(value).length > 128) return "too_long";
  if (value.includes(",")) return "comma";
  if (new RegExp(`[${ASCII_CONTROL_CHARS}]`).test(value) || hasUnicodeControls(value)) {
    return "control";
  }
  return null;
}

export function validateStyleEditOperations(
  state: StyleEditOperationState
): StyleEditOperationValidation {
  const hasEnabledOperation = state.fontFamilyEnabled || state.fontSizeEnabled;
  const targetFontError = state.fontFamilyEnabled
    ? validateStyleFontFamily(state.targetFontFamily)
    : null;
  const sourceFontError =
    state.fontFamilyEnabled && state.sourceFilterEnabled
      ? validateStyleFontFamily(state.sourceFontFamily)
      : null;
  const parsedSize = state.fontSizeEnabled ? parseFiniteNumberText(state.targetFontSize) : null;
  const fontSizeInvalid =
    state.fontSizeEnabled &&
    (parsedSize === null ||
      parsedSize < STYLE_EDIT_MIN_FONT_SIZE ||
      parsedSize > STYLE_EDIT_MAX_FONT_SIZE);

  return {
    targetFontFamily:
      state.fontFamilyEnabled && targetFontError === null ? state.targetFontFamily : null,
    sourceFontFamily:
      state.fontFamilyEnabled && state.sourceFilterEnabled && sourceFontError === null
        ? state.sourceFontFamily
        : null,
    targetFontSize: state.fontSizeEnabled && !fontSizeInvalid ? parsedSize : null,
    targetFontError,
    sourceFontError,
    fontSizeInvalid,
    hasEnabledOperation,
    valid:
      hasEnabledOperation &&
      targetFontError === null &&
      sourceFontError === null &&
      !fontSizeInvalid,
  };
}

export function isStyleEditWriteDisabled(options: {
  fileCount: number;
  busy: boolean;
  operationsValid: boolean;
  effectiveSelectedRowCount: number;
}): boolean {
  return (
    options.fileCount === 0 ||
    options.busy ||
    !options.operationsValid ||
    options.effectiveSelectedRowCount === 0
  );
}

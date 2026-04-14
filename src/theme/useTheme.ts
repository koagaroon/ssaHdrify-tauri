/**
 * Theme hook — three-mode system matching the Python version:
 *   "auto"  — follow system preference (Windows: registry, others: prefers-color-scheme)
 *   "light" — force light
 *   "dark"  — force dark
 *
 * Context exposes both the user's mode choice and the resolved appearance.
 */
import { createContext, useContext } from "react";

/** What the user chose */
export type ThemeMode = "auto" | "light" | "dark";
/** What actually renders */
export type ThemeAppearance = "light" | "dark";

export interface ThemeContextValue {
  mode: ThemeMode;
  appearance: ThemeAppearance;
  setMode: (mode: ThemeMode) => void;
}

export const ThemeContext = createContext<ThemeContextValue>({
  mode: "light",
  appearance: "light",
  setMode: () => {},
});

export function useTheme() {
  return useContext(ThemeContext);
}

/**
 * Theme hook — an independent palette and appearance mode:
 *   "auto"  — follow the system preference via prefers-color-scheme
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
export type ThemePalette = "cool" | "warm";

export interface ThemeContextValue {
  mode: ThemeMode;
  appearance: ThemeAppearance;
  palette: ThemePalette;
  setMode: (mode: ThemeMode) => void;
  setPalette: (palette: ThemePalette) => void;
}

export const ThemeContext = createContext<ThemeContextValue>({
  mode: "light",
  appearance: "light",
  palette: "cool",
  setMode: () => {},
  setPalette: () => {},
});

export function useTheme() {
  return useContext(ThemeContext);
}

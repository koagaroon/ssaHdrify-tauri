/**
 * Theme context provider — independent gray palette and auto / light / dark mode.
 *
 * "auto" listens to prefers-color-scheme media query and updates
 * the resolved appearance in real time. Persists both choices independently.
 */
import { useCallback, useEffect, useLayoutEffect, useMemo, useState, type ReactNode } from "react";
import { ThemeContext, type ThemeMode, type ThemeAppearance, type ThemePalette } from "./useTheme";

// Keep the released mode key and values so upgrades retain the user's appearance.
const STORAGE_KEY = "ssahdrify-theme";
const PALETTE_STORAGE_KEY = "ssahdrify-palette";

function loadMode(): ThemeMode {
  const stored = (() => {
    try {
      return localStorage.getItem(STORAGE_KEY);
    } catch {
      return null;
    }
  })();
  if (stored === "auto" || stored === "light" || stored === "dark") return stored;
  return "light"; // default
}

function loadPalette(): ThemePalette {
  try {
    const stored = localStorage.getItem(PALETTE_STORAGE_KEY);
    if (stored === "cool" || stored === "warm") return stored;
  } catch {
    // Storage can be unavailable; changing appearance must still work in memory.
  }
  return "cool";
}

function getSystemPreference(): ThemeAppearance {
  try {
    if (window.matchMedia?.("(prefers-color-scheme: dark)").matches) return "dark";
  } catch {
    // Non-browser or hardened WebView hosts can reject media queries.
  }
  return "light";
}

export default function ThemeProvider({ children }: { children: ReactNode }) {
  const [mode, setModeState] = useState<ThemeMode>(loadMode);
  const [palette, setPaletteState] = useState<ThemePalette>(loadPalette);
  const [systemPref, setSystemPref] = useState<ThemeAppearance>(getSystemPreference);

  const setMode = useCallback((next: ThemeMode) => {
    setModeState(next);
    try {
      localStorage.setItem(STORAGE_KEY, next);
    } catch {
      // Keep the live mode even when persistence is unavailable.
    }
  }, []);

  const setPalette = useCallback((next: ThemePalette) => {
    setPaletteState(next);
    try {
      localStorage.setItem(PALETTE_STORAGE_KEY, next);
    } catch {
      // Keep the live palette even when persistence is unavailable.
    }
  }, []);

  // Listen for system preference changes (for auto mode)
  useEffect(() => {
    try {
      const mq = window.matchMedia?.("(prefers-color-scheme: dark)");
      if (!mq) return;
      const handler = (e: MediaQueryListEvent) => {
        setSystemPref(e.matches ? "dark" : "light");
      };
      mq.addEventListener("change", handler);
      return () => mq.removeEventListener("change", handler);
    } catch {
      // Retain the initial appearance if this host cannot observe system changes.
    }
  }, []);

  const appearance: ThemeAppearance = useMemo(() => {
    if (mode === "auto") return systemPref;
    return mode;
  }, [mode, systemPref]);

  // Apply restored choices before React's first paint.
  useLayoutEffect(() => {
    document.documentElement.setAttribute("data-theme", appearance);
    document.documentElement.setAttribute("data-palette", palette);
  }, [appearance, palette]);

  const contextValue = useMemo(
    () => ({ mode, appearance, palette, setMode, setPalette }),
    [mode, appearance, palette, setMode, setPalette]
  );

  return <ThemeContext.Provider value={contextValue}>{children}</ThemeContext.Provider>;
}

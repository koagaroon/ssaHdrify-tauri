/**
 * Theme context provider — three modes: auto / light / dark.
 *
 * "auto" listens to prefers-color-scheme media query and updates
 * the resolved appearance in real time. Persists mode to localStorage.
 */
import { useState, useEffect, useMemo, type ReactNode } from "react";
import { ThemeContext, type ThemeMode, type ThemeAppearance } from "./useTheme";

const STORAGE_KEY = "ssahdrify-theme";

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
  const [systemPref, setSystemPref] = useState<ThemeAppearance>(getSystemPreference);

  const setMode = (next: ThemeMode) => {
    setModeState(next);
    try {
      localStorage.setItem(STORAGE_KEY, next);
    } catch {
      // Keep the live mode even when persistence is unavailable.
    }
  };

  // Listen for system preference changes (for auto mode)
  useEffect(() => {
    if (!window.matchMedia) return;
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const handler = (e: MediaQueryListEvent) => {
      setSystemPref(e.matches ? "dark" : "light");
    };
    mq.addEventListener("change", handler);
    return () => mq.removeEventListener("change", handler);
  }, []);

  const appearance: ThemeAppearance = useMemo(() => {
    if (mode === "auto") return systemPref;
    return mode;
  }, [mode, systemPref]);

  // Apply data-theme to <html>
  useEffect(() => {
    document.documentElement.setAttribute("data-theme", appearance);
  }, [appearance]);

  return (
    <ThemeContext.Provider value={{ mode, appearance, setMode }}>{children}</ThemeContext.Provider>
  );
}

/**
 * I18n context provider — detects system language on first launch,
 * persists user choice to localStorage.
 */
import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import { I18nContext } from "./useI18n";
import type { Lang } from "./strings";

const STORAGE_KEY = "ssahdrify-lang";

function loadLang(): Lang {
  const stored = (() => {
    try {
      return localStorage.getItem(STORAGE_KEY);
    } catch {
      return null;
    }
  })();
  if (stored === "en" || stored === "zh") return stored;
  // First launch defaults to Chinese by project choice: the primary user
  // base for fan-sub workflows runs Chinese-language subtitles, and most
  // first-time users land via Chinese-language community channels. We
  // skip navigator.language detection on purpose — Windows users in CN
  // commonly run an English-display OS while still wanting CN UI, so a
  // language-detect default would mis-fit the modal user. Users can
  // switch to English via the header toggle; the choice persists in
  // localStorage.
  return "zh";
}

export default function I18nProvider({ children }: { children: ReactNode }) {
  const [lang, setLangState] = useState<Lang>(loadLang);

  // Stabilize setLang so it doesn't churn the context value identity on
  // every parent render — a fresh function each render plus a fresh
  // context value would force every useI18n consumer to re-render even
  // when `lang` didn't change. Mirror StatusProvider's memoization
  // shape.
  const setLang = useCallback((next: Lang) => {
    setLangState(next);
    try {
      localStorage.setItem(STORAGE_KEY, next);
    } catch {
      // Storage can be disabled in hardened/sandboxed WebView profiles.
      // The in-memory state above still applies for this session.
    }
  }, []);

  // Reflect the active locale onto <html lang="…"> so CSS `:lang()`
  // selectors (font stack switching in index.css) and screen readers
  // both see the current UI language.
  useEffect(() => {
    document.documentElement.setAttribute("lang", lang);
  }, [lang]);

  // Stable context value — re-created only when `lang` actually
  // changes. Without this, every parent render would hand consumers a
  // fresh object identity even if `lang` was unchanged.
  const value = useMemo(() => ({ lang, setLang }), [lang, setLang]);

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

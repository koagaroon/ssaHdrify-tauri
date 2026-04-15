/**
 * I18n context provider — detects system language on first launch,
 * persists user choice to localStorage.
 */
import { useState, type ReactNode } from "react";
import { I18nContext } from "./useI18n";
import type { Lang } from "./strings";

const STORAGE_KEY = "ssahdrify-lang";

function loadLang(): Lang {
  const stored = localStorage.getItem(STORAGE_KEY);
  if (stored === "en" || stored === "zh") return stored;
  // First launch (no localStorage): always Chinese regardless of system language.
  // User can switch to English manually; that choice persists via localStorage.
  return "zh";
}

export default function I18nProvider({ children }: { children: ReactNode }) {
  const [lang, setLangState] = useState<Lang>(loadLang);

  const setLang = (next: Lang) => {
    setLangState(next);
    localStorage.setItem(STORAGE_KEY, next);
  };

  return (
    <I18nContext.Provider value={{ lang, setLang }}>
      {children}
    </I18nContext.Provider>
  );
}

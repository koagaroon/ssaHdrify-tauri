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
  // First launch defaults to Chinese by project choice — not a fallback for
  // undetected system language. Users can switch to English via the header
  // toggle; the choice persists in localStorage.
  return "zh";
}

export default function I18nProvider({ children }: { children: ReactNode }) {
  const [lang, setLangState] = useState<Lang>(loadLang);

  const setLang = (next: Lang) => {
    setLangState(next);
    localStorage.setItem(STORAGE_KEY, next);
  };

  return <I18nContext.Provider value={{ lang, setLang }}>{children}</I18nContext.Provider>;
}

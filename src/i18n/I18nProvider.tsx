/**
 * I18n context provider — detects system language on first launch,
 * persists user choice to localStorage.
 */
import { useState, useEffect, type ReactNode } from "react";
import { I18nContext } from "./useI18n";
import type { Lang } from "./strings";

const STORAGE_KEY = "ssahdrify-lang";

function detectSystemLang(): Lang {
  const nav = navigator.language || "";
  // Chinese variants: zh, zh-CN, zh-TW, zh-Hans, zh-Hant
  if (nav.startsWith("zh")) return "zh";
  return "en";
}

function loadLang(): Lang {
  const stored = localStorage.getItem(STORAGE_KEY);
  if (stored === "en" || stored === "zh") return stored;
  return detectSystemLang();
}

export default function I18nProvider({ children }: { children: ReactNode }) {
  const [lang, setLangState] = useState<Lang>(loadLang);

  const setLang = (next: Lang) => {
    setLangState(next);
    localStorage.setItem(STORAGE_KEY, next);
  };

  // Sync on mount (in case localStorage changed externally)
  useEffect(() => {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored === "en" || stored === "zh") {
      setLangState(stored);
    }
  }, []);

  return (
    <I18nContext.Provider value={{ lang, setLang }}>
      {children}
    </I18nContext.Provider>
  );
}

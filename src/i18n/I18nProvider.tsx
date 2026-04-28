/**
 * I18n context provider — detects system language on first launch,
 * persists user choice to localStorage.
 */
import { useEffect, useState, type ReactNode } from "react";
import { I18nContext } from "./useI18n";
import type { Lang } from "./strings";

const STORAGE_KEY = "ssahdrify-lang";

function loadLang(): Lang {
  const stored = localStorage.getItem(STORAGE_KEY);
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

  const setLang = (next: Lang) => {
    setLangState(next);
    localStorage.setItem(STORAGE_KEY, next);
  };

  // Reflect the active locale onto <html lang="…"> so CSS `:lang()`
  // selectors (font stack switching in index.css) and screen readers
  // both see the current UI language.
  useEffect(() => {
    document.documentElement.setAttribute("lang", lang);
  }, [lang]);

  return <I18nContext.Provider value={{ lang, setLang }}>{children}</I18nContext.Provider>;
}

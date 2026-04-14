/**
 * i18n hook — provides t() translation function with parametric substitution.
 *
 * Usage:
 *   const { t, lang, setLang } = useI18n();
 *   t("msg_done", fileName)  →  "Done: subtitle.ass"
 */
import { createContext, useContext } from "react";
import { strings, type Lang } from "./strings";

export interface I18nContextValue {
  lang: Lang;
  setLang: (lang: Lang) => void;
}

export const I18nContext = createContext<I18nContextValue>({
  lang: "en",
  setLang: () => {},
});

/**
 * Translate a key, substituting {0}, {1}, ... with additional arguments.
 */
function translate(lang: Lang, key: string, ...args: (string | number)[]): string {
  const entry = strings[key];
  if (!entry) return key; // fallback: show key name

  let text = entry[lang] ?? entry.en ?? key;
  for (let i = 0; i < args.length; i++) {
    text = text.replace(`{${i}}`, String(args[i]));
  }
  return text;
}

export function useI18n() {
  const { lang, setLang } = useContext(I18nContext);

  const t = (key: string, ...args: (string | number)[]) =>
    translate(lang, key, ...args);

  return { t, lang, setLang };
}

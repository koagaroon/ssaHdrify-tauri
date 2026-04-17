/**
 * i18n hook — provides t() translation function with parametric substitution.
 *
 * Usage:
 *   const { t, lang, setLang } = useI18n();
 *   t("msg_done", fileName)  →  "Done: subtitle.ass"
 */
import { createContext, useCallback, useContext } from "react";
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
 *
 * NOTE: Translation args may contain user-controlled content (file paths, errors).
 * Safe because React JSX renders as text nodes, not HTML. Do NOT use with dangerouslySetInnerHTML.
 */
function translate(lang: Lang, key: string, ...args: (string | number)[]): string {
  const entry = strings[key];
  if (!entry) return key; // fallback: show key name

  let text = entry[lang] ?? entry.en ?? key;
  for (let i = 0; i < args.length; i++) {
    text = text.replace(new RegExp(`\\{${i}\\}`, "g"), String(args[i]));
  }
  return text;
}

export function useI18n() {
  const { lang, setLang } = useContext(I18nContext);

  const t = useCallback(
    (key: string, ...args: (string | number)[]) => translate(lang, key, ...args),
    [lang]
  );

  return { t, lang, setLang };
}

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
// Pre-compiled — shared across every translate() call, no per-call allocation.
const PLACEHOLDER_RE = /\{(\d+)\}/g;

function translate(lang: Lang, key: string, ...args: (string | number)[]): string {
  const entry = strings[key];
  if (!entry) return key; // fallback: show key name

  const text = entry[lang] ?? entry.en ?? key;
  if (args.length === 0) return text;
  // Use the function replacer form so substitution values containing literal
  // `$1` / `$&` are NOT interpreted as regex backreferences. Passing a
  // function closes that injection path — paths like "C:\$RECYCLE.BIN" would
  // otherwise trigger accidental expansion via String.replace semantics.
  return text.replace(PLACEHOLDER_RE, (match, idxStr: string) => {
    const idx = Number(idxStr);
    return idx < args.length ? String(args[idx]) : match;
  });
}

export function useI18n() {
  const { lang, setLang } = useContext(I18nContext);

  const t = useCallback(
    (key: string, ...args: (string | number)[]) => translate(lang, key, ...args),
    [lang]
  );

  return { t, lang, setLang };
}

import { describe, expect, it } from "vitest";
import { strings, type Lang } from "./strings";

const LANGS: Lang[] = ["en", "zh"];
const PLACEHOLDER_RE = /\{(\d+)\}/g;

function placeholders(value: string): string[] {
  return [...value.matchAll(PLACEHOLDER_RE)].map((m) => m[1]).sort();
}

describe("i18n strings", () => {
  it("keeps every visible string bilingual and placeholder-compatible", () => {
    for (const [key, entry] of Object.entries(strings)) {
      for (const lang of LANGS) {
        expect(entry[lang], `${key}.${lang}`).toBeTypeOf("string");
        expect(entry[lang].trim().length, `${key}.${lang}`).toBeGreaterThan(0);
      }
      expect(placeholders(entry.zh), `${key} placeholders`).toEqual(placeholders(entry.en));
    }
  });
});

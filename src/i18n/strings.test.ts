import { describe, expect, it } from "vitest";
import { strings, type Lang } from "./strings";
import { translate } from "./useI18n";

const LANGS: Lang[] = ["en", "zh"];
const PLACEHOLDER_RE = /\{(\d+)\}/g;

/**
 * Extract placeholder ids as a sorted MULTISET (Array, not Set). Sort
 * is for order-independence; duplicates are preserved so
 * `placeholders("{0} {0}")` returns `["0", "0"]` and would fail
 * `toEqual` against an `en` value with only one `{0}`. Round 1
 * F1.N-R1-7 raised concern that this might be set-not-multiset; the
 * `.sort()` is order-only and `toEqual` does length-aware deep array
 * equality, so multiset semantics hold.
 */
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

describe("translate runtime substitution", () => {
  it("substitutes {N} placeholders with stringified args", () => {
    const out = translate("en", "msg_overwrite_confirm", 2, 5);
    expect(out).toContain("2");
    expect(out).toContain("5");
    expect(out).not.toContain("{0}");
    expect(out).not.toContain("{1}");
  });

  it("returns the bare key for unknown ids and leaves placeholders untouched when no args", () => {
    expect(translate("en", "definitely_not_a_real_key")).toBe("definitely_not_a_real_key");
    // Sanity: with zero args, placeholders stay literal so the caller
    // sees them and can route in args. The substitution engine must not
    // silently drop them.
    const literal = translate("en", "msg_overwrite_confirm");
    expect(literal).toMatch(/\{0\}/);
  });

  it("does NOT expand $1 / $& backreferences in arg values", () => {
    // Regression guard for the `String.replace` injection class — the
    // function-form replacer is meant to keep `$1` literal in args. A
    // path like "C:\\$RECYCLE.BIN" must not get any portion expanded.
    const out = translate("en", "msg_overwrite_confirm", "C:\\$RECYCLE.BIN", "$&-glob");
    expect(out).toContain("C:\\$RECYCLE.BIN");
    expect(out).toContain("$&-glob");
  });
});

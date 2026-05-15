/**
 * dedup-helpers tests — pin the sanitizer contracts that protect the
 * OS-native ask() dialog body from Trojan-Source / smuggled-newline
 * attacks. The helpers themselves are small; what we pin here is the
 * exact codepoint coverage so a future "simplify" pass can't quietly
 * drop a class without a test failure.
 */
import { describe, it, expect } from "vitest";

import { sanitizeForDialog, sanitizeError } from "./dedup-helpers";

describe("sanitizeForDialog", () => {
  it("strips BiDi / zero-width controls (Round 6 W6.2 parity set)", () => {
    // U+202E RIGHT-TO-LEFT OVERRIDE — the classic Trojan-Source vector.
    // U+200B ZERO WIDTH SPACE — invisible separator.
    // U+FEFF ZWNBSP / BOM-in-middle. Use escape form so ESLint's
    // no-irregular-whitespace rule doesn't trip on literal codepoints.
    expect(sanitizeForDialog("a\u{202E}b")).toBe("ab");
    expect(sanitizeForDialog("a\u{200B}b")).toBe("ab");
    expect(sanitizeForDialog("a\u{FEFF}b")).toBe("ab");
  });

  it("strips C0 + DEL + C1 control range (Round 10 N-R10-026 widening, was Round 8 A-R8-A4-24 \\r\\n-only)", () => {
    // Round 8 A-R8-A4-24 scrubbed only \n / \r; Round 10 N-R10-026
    // widened the contract to the full C0 (\x00-\x1f) + DEL (\x7f) +
    // C1 (\x80-\x9f) range so the test must exercise the wider span,
    // not just the original \r\n smuggle.
    //
    // A fan-sub filename with an embedded newline can fake additional
    // confirm lines in the OS-native dialog body (\r\n smuggle, the
    // classic case). \t produces uneven indentation; \0 can truncate
    // Win32 TaskDialog text at the NUL byte; ESC (\x1b) can drive
    // terminal cursor manipulation on stderr surfaces; \x7f (DEL) and
    // C1 codepoints round out the parity with the path-validation
    // regex.
    expect(sanitizeForDialog("episode\n.ass")).toBe("episode.ass");
    expect(sanitizeForDialog("episode\r.ass")).toBe("episode.ass");
    expect(sanitizeForDialog("a\r\nb")).toBe("ab");
    expect(sanitizeForDialog("safe.ass\nDelete C:\\* ?")).toBe("safe.assDelete C:\\* ?");
    // Widened-range pins. Use escape forms to keep the test source
    // readable against ESLint's no-irregular-whitespace rule.
    expect(sanitizeForDialog("a\x00b")).toBe("ab");
    expect(sanitizeForDialog("a\tb")).toBe("ab");
    expect(sanitizeForDialog("a\x1bb")).toBe("ab");
    expect(sanitizeForDialog("a\x7fb")).toBe("ab");
    expect(sanitizeForDialog("a\x85b")).toBe("ab"); // NEL (C1)
    expect(sanitizeForDialog("a\x9fb")).toBe("ab"); // C1 upper bound
  });

  it("preserves ordinary CJK and Latin text", () => {
    expect(sanitizeForDialog("Episode 1.ass")).toBe("Episode 1.ass");
    expect(sanitizeForDialog("第一话.ass")).toBe("第一话.ass");
  });
});

describe("sanitizeError", () => {
  it("extracts Error.message and scrubs BiDi / line breaks", () => {
    const err = new Error("evil\u{202E}txt.ass\nrm -rf");
    // The order matches the helper: extract message → sanitizeForDialog.
    expect(sanitizeError(err)).toBe("eviltxt.assrm -rf");
  });

  it("falls back to String(e) for non-Error throws", () => {
    expect(sanitizeError("bare string\n.danger")).toBe("bare string.danger");
    expect(sanitizeError(42)).toBe("42");
  });

  it("scrubs BiDi codepoints on non-Error throws too (Round 10 N-R10-021)", () => {
    // Pre-R10 the non-Error tests only covered ASCII payloads
    // ("bare string\n.danger", 42), so a future refactor that
    // bypassed sanitizeForDialog on the non-Error branch (e.g.,
    // `return e instanceof Error ? sanitizeForDialog(e.message) :
    // String(e)`) would have passed every assertion. The single
    // BiDi-bearing string-throw pins the contract that both branches
    // go through the scrub.
    expect(sanitizeError("evil\u{202E}txt")).toBe("eviltxt");
    // C0 control chars on non-Error throw — sanitizeForDialog's
    // R10-widened scrub catches these too.
    expect(sanitizeError("a\x00b\tc")).toBe("abc");
  });
});

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

  it("strips ASCII line breaks \\n / \\r (Round 8 A-R8-A4-24)", () => {
    // A fan-sub filename with an embedded newline could fake additional
    // confirm lines in the OS-native dialog body. U+2028 / U+2029 are
    // already covered by the BiDi set; this closes the ASCII gap.
    expect(sanitizeForDialog("episode\n.ass")).toBe("episode.ass");
    expect(sanitizeForDialog("episode\r.ass")).toBe("episode.ass");
    expect(sanitizeForDialog("a\r\nb")).toBe("ab");
    // The smuggling payload an attacker would actually craft.
    expect(sanitizeForDialog("safe.ass\nDelete C:\\* ?")).toBe("safe.assDelete C:\\* ?");
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
});

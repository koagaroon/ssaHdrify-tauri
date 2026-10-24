import { describe, expect, it } from "vitest";
import { LICENSE_NOTICES } from "./license-notices";
import {
  LICENSE_PAGE_SIZE,
  LICENSE_SECTION_SEPARATOR,
  licensePages,
  licenseSections,
} from "./license-pages";

describe("license reader pagination", () => {
  it("keeps the complete embedded inventory available without putting it all on one page", () => {
    for (const notice of LICENSE_NOTICES) {
      const sections = licenseSections(notice.text);
      expect(sections.map((section) => section.text).join(LICENSE_SECTION_SEPARATOR)).toBe(
        notice.text
      );
      for (const section of sections) {
        const pages = licensePages(section.text);
        expect(pages.join("")).toBe(section.text);
        expect(pages.every((page) => page.length <= LICENSE_PAGE_SIZE)).toBe(true);
      }
    }
  });

  it("accepts one full page and splits one character over the limit", () => {
    expect(licensePages("")).toEqual([""]);
    expect(licensePages("a".repeat(LICENSE_PAGE_SIZE))).toHaveLength(1);
    expect(licensePages("a".repeat(LICENSE_PAGE_SIZE + 1))).toEqual([
      "a".repeat(LICENSE_PAGE_SIZE),
      "a",
    ]);
  });

  it("preserves whitespace and keeps Unicode surrogate pairs together at a page boundary", () => {
    const text = "a".repeat(LICENSE_PAGE_SIZE - 1) + "😀\n\n  next line  \n";
    const pages = licensePages(text);
    expect(pages[0]).toBe("a".repeat(LICENSE_PAGE_SIZE - 1));
    expect(pages[1]).toBe("😀\n\n  next line  \n");
    expect(pages.join("")).toBe(text);
  });

  it("uses a nearby line ending without dropping the newline", () => {
    const firstLine = "a".repeat(LICENSE_PAGE_SIZE / 2) + "\n";
    const pages = licensePages(firstLine + "b".repeat(LICENSE_PAGE_SIZE));
    expect(pages).toEqual([firstLine, "b".repeat(LICENSE_PAGE_SIZE)]);
  });
});

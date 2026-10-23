export const LICENSE_PAGE_SIZE = 24_000;
export const LICENSE_SECTION_SEPARATOR = "\n\n" + "=".repeat(72) + "\n\n";

export function licenseSections(text: string): { title: string; text: string }[] {
  return text.split(LICENSE_SECTION_SEPARATOR).map((section) => ({
    title: section.split("\n", 1)[0] || "—",
    text: section,
  }));
}

export function licensePages(text: string): string[] {
  const pages: string[] = [];
  for (let start = 0; start < text.length;) {
    let end = Math.min(start + LICENSE_PAGE_SIZE, text.length);
    if (end < text.length) {
      const newline = text.lastIndexOf("\n", end - 1);
      if (newline >= start + LICENSE_PAGE_SIZE / 2) end = newline + 1;
      // Keep a Unicode character together when a very long line needs a hard split.
      if (/[\uD800-\uDBFF]/u.test(text[end - 1]!) && /[\uDC00-\uDFFF]/u.test(text[end]!)) {
        end--;
      }
    }
    pages.push(text.slice(start, end));
    start = end;
  }
  return pages.length ? pages : [""];
}

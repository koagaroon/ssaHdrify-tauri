import { describe, expect, it } from "vitest";

import {
  formatFontScanBytes,
  LARGE_FONT_SCAN_BYTES_WARNING_THRESHOLD,
  LARGE_FONT_SCAN_FILE_WARNING_THRESHOLD,
  shouldWarnLargeFontScan,
} from "./font-source-warning";

describe("large font source warning helpers", () => {
  it("warns at the file-count or total-byte threshold", () => {
    expect(
      shouldWarnLargeFontScan({
        fontFiles: LARGE_FONT_SCAN_FILE_WARNING_THRESHOLD - 1,
        totalBytes: LARGE_FONT_SCAN_BYTES_WARNING_THRESHOLD - 1,
      })
    ).toBe(false);
    expect(
      shouldWarnLargeFontScan({
        fontFiles: LARGE_FONT_SCAN_FILE_WARNING_THRESHOLD,
        totalBytes: 1,
      })
    ).toBe(true);
    expect(
      shouldWarnLargeFontScan({
        fontFiles: 1,
        totalBytes: LARGE_FONT_SCAN_BYTES_WARNING_THRESHOLD,
      })
    ).toBe(true);
  });

  it("formats byte counts with binary units for the confirmation dialog", () => {
    expect(formatFontScanBytes(5 * 1024 * 1024 * 1024)).toBe("5.0 GiB");
    expect(formatFontScanBytes(128 * 1024 * 1024)).toBe("128.0 MiB");
    expect(formatFontScanBytes(4096)).toBe("4.0 KiB");
  });
});

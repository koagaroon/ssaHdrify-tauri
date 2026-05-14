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
    // Round 10 N-R10-020: pair the at-threshold tests above with
    // over-threshold counter-tests. The original triple covered
    // (below-below, at-file, at-bytes) — a regression flipping the
    // gate to `>` instead of `>=` would silently weaken the cap, and
    // the at-threshold tests alone can't distinguish that direction.
    // Over-threshold pins the inequality from the other side.
    expect(
      shouldWarnLargeFontScan({
        fontFiles: LARGE_FONT_SCAN_FILE_WARNING_THRESHOLD + 1,
        totalBytes: 1,
      })
    ).toBe(true);
    expect(
      shouldWarnLargeFontScan({
        fontFiles: 1,
        totalBytes: LARGE_FONT_SCAN_BYTES_WARNING_THRESHOLD + 1,
      })
    ).toBe(true);
  });

  it("formats byte counts with binary units for the confirmation dialog", () => {
    expect(formatFontScanBytes(5 * 1024 * 1024 * 1024)).toBe("5.0 GiB");
    expect(formatFontScanBytes(128 * 1024 * 1024)).toBe("128.0 MiB");
    expect(formatFontScanBytes(4096)).toBe("4.0 KiB");
  });

  it("handles bytes-only and unit-boundary cases", () => {
    // Bare-byte branch (< 1 KiB).
    expect(formatFontScanBytes(0)).toBe("0 B");
    expect(formatFontScanBytes(512)).toBe("512 B");
    expect(formatFontScanBytes(1023)).toBe("1023 B");
    // Unit boundaries — each lower edge promotes to the next-larger unit.
    expect(formatFontScanBytes(1024)).toBe("1.0 KiB");
    expect(formatFontScanBytes(1024 * 1024)).toBe("1.0 MiB");
    expect(formatFontScanBytes(1024 * 1024 * 1024)).toBe("1.0 GiB");
  });
});

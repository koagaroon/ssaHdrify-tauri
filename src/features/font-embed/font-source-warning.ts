import type { FontScanPreflight } from "../../lib/tauri-api";

// Calibrated against the manual font-scan test buckets:
//   SMALL ≈ 100, MED ≈ 500, TTC ≈ 800, BIG ≈ 5,723, XL ≈ 17,942 fonts.
// 5_000 sits just below BIG so SMALL/MED/TTC all scan without warning
// while BIG/XL warn the user that a long scan is about to start.
export const LARGE_FONT_SCAN_FILE_WARNING_THRESHOLD = 5_000;

// XL is ~54 GiB on disk; 5 GiB warns well before that surface. Real-world
// "Fonts" folders top out around 5–10 GiB even for serious type collectors.
export const LARGE_FONT_SCAN_BYTES_WARNING_THRESHOLD = 5 * 1024 * 1024 * 1024;

export function shouldWarnLargeFontScan(preflight: FontScanPreflight): boolean {
  return (
    preflight.fontFiles >= LARGE_FONT_SCAN_FILE_WARNING_THRESHOLD ||
    preflight.totalBytes >= LARGE_FONT_SCAN_BYTES_WARNING_THRESHOLD
  );
}

export function formatFontScanBytes(bytes: number): string {
  const gib = bytes / (1024 * 1024 * 1024);
  if (gib >= 1) return `${gib.toFixed(1)} GiB`;
  const mib = bytes / (1024 * 1024);
  if (mib >= 1) return `${mib.toFixed(1)} MiB`;
  const kib = bytes / 1024;
  if (kib >= 1) return `${kib.toFixed(1)} KiB`;
  return `${bytes} B`;
}

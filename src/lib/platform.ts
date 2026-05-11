/**
 * Runtime platform identification.
 *
 * Set once at module load by checking the test runner's `process.platform`
 * (Vitest runs in Node) and falling back to `navigator.userAgent` (Tauri
 * WebView2 — chromium-based, reliably reports the host OS). Default is
 * POSIX-safe when neither is available, so a future headless test env
 * doesn't accidentally inherit Windows assumptions.
 *
 * Used by path-handling helpers that previously assumed Windows
 * semantics unconditionally. Two distinct gates exist because the
 * problems they solve are distinct:
 *
 * - `isWindowsRuntime` — gates the "backslash is a path separator"
 *   heuristic. On Windows it is; on POSIX `\` is a valid filename
 *   character and treating it as a separator misroutes outputs
 *   (Codex edb0e74f / 8850ede7).
 *
 * - `isCaseInsensitiveFs` — gates the duplicate-output-key lowercasing
 *   that catches case-only collisions (`Episode.ass` vs `episode.ass`).
 *   True on Windows (NTFS) and macOS (APFS / HFS+ both default to
 *   case-insensitive); false on Linux ext4 / btrfs / xfs which are
 *   case-sensitive (Codex dd2d9554). Linux users running macOS-formatted
 *   external drives are a <1% edge that has to opt in another way.
 */

function nodePlatform(): string | undefined {
  // Vitest test env exposes Node's `process`. Tauri WebView2 does not —
  // `process` is undefined there, which is why we fall through.
  if (typeof process !== "undefined" && process.platform) {
    return process.platform;
  }
  return undefined;
}

function browserUserAgent(): string | undefined {
  if (typeof navigator !== "undefined" && navigator.userAgent) {
    return navigator.userAgent;
  }
  return undefined;
}

export const isWindowsRuntime: boolean = (() => {
  const np = nodePlatform();
  if (np) return np === "win32";
  const ua = browserUserAgent();
  if (ua) return /Windows/i.test(ua);
  return false;
})();

export const isCaseInsensitiveFs: boolean = (() => {
  const np = nodePlatform();
  if (np) return np === "win32" || np === "darwin";
  const ua = browserUserAgent();
  if (ua) return /Windows|Mac OS|Macintosh/i.test(ua);
  return false;
})();

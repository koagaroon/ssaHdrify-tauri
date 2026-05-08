/**
 * Shared output-path validation helpers.
 *
 * Extracted from `output-naming.ts`'s HDR resolver so that Shift and
 * Embed (CLI + GUI sides) can apply the same safety checks. Before
 * this extraction, HDR's resolver was the canonical implementation
 * and the others rolled their own minimal byte-illegal-char checks —
 * `CON.ass`, `..` segments, MAX_PATH overflow, drive-relative paths
 * passed through. Aligning them here closes a real-but-narrow attack
 * surface (CLI receives untrusted argv; GUI takes user-typed
 * templates) and gives consistent error messaging across all three
 * commands.
 *
 * The HDR resolver still owns its `.ass` extension check and template-
 * variable substitution; this module only covers the per-filename and
 * per-path safety checks that are common to all three.
 */

// ── Windows reserved names ─────────────────────────────────
// Forbidden on Windows regardless of extension (NT object-namespace
// reservations: legacy device names that the kernel routes specially).
export const WINDOWS_RESERVED_NAMES = new Set([
  "CON",
  "PRN",
  "AUX",
  "NUL",
  "COM1",
  "COM2",
  "COM3",
  "COM4",
  "COM5",
  "COM6",
  "COM7",
  "COM8",
  "COM9",
  "LPT1",
  "LPT2",
  "LPT3",
  "LPT4",
  "LPT5",
  "LPT6",
  "LPT7",
  "LPT8",
  "LPT9",
  "CONIN$",
  "CONOUT$",
]);

/**
 * Match characters illegal in NTFS filenames. Includes control chars
 * (which Windows would silently truncate at) plus the explicit NTFS-
 * reserved punctuation and path separators (so a "filename" with a
 * separator can't sneak through).
 *
 * Cross-platform note: `:` is technically valid on macOS / Linux but
 * we reject it everywhere — this app's primary platform is Windows
 * and outputs cross machines, so the strictest filesystem's rules win.
 */
// eslint-disable-next-line no-control-regex -- intentional: reject control chars in filenames
export const ILLEGAL_FILENAME_CHARS = /[\x00-\x1f\x7f<>:"|?*\\/]/;

/**
 * Validate a single output filename (no path separators) for safety.
 * Caller is responsible for stripping the directory portion before
 * calling.
 *
 * Throws on:
 *   - empty / whitespace-only filename
 *   - illegal characters (control / NTFS-reserved / separators)
 *   - Windows reserved name (CON, PRN, etc., case-insensitive,
 *     applied to the stem with trailing whitespace + dots stripped)
 */
export function assertSafeOutputFilename(filename: string): void {
  if (!filename.trim()) {
    throw new Error("Template resolves to empty filename");
  }
  if (ILLEGAL_FILENAME_CHARS.test(filename)) {
    throw new Error(`Output filename contains illegal characters: ${filename}`);
  }
  // Windows reserves these names regardless of extension: per
  // Microsoft, "NUL.txt" and "NUL.tar.gz" both resolve to the NUL
  // device. So check the FIRST segment (everything before the first
  // dot), not the final stem. Also strip trailing whitespace and dots
  // because `CON ` and `CON.` resolve to the device too.
  const firstDot = filename.indexOf(".");
  const firstSegment = (firstDot > 0 ? filename.slice(0, firstDot) : filename).replace(
    /[\s.]+$/,
    ""
  );
  if (WINDOWS_RESERVED_NAMES.has(firstSegment.toUpperCase())) {
    throw new Error(`Output filename is a Windows reserved name: ${firstSegment}`);
  }
}

/**
 * Validate a full output path against the input path's directory.
 * Throws on traversal, directory escape, MAX_PATH overflow, and
 * self-overwrite.
 *
 * Both arguments may use either separator style; the helper normalizes
 * to forward slashes internally before comparing.
 */
export function assertSafeOutputPath(outputPath: string, inputPath: string): void {
  const normalizedOutput = outputPath.replace(/\\/g, "/");
  const normalizedInput = inputPath.replace(/\\/g, "/");
  const inputDirEnd = normalizedInput.lastIndexOf("/");
  if (inputDirEnd < 0) {
    throw new Error("Input path has no directory component");
  }
  const inputDir = normalizedInput.slice(0, inputDirEnd);

  // Path traversal — `..` as a path component, not as a substring of
  // a longer name like `..foo` (which is legal).
  if (/(^|\/)\.\.($|\/)/.test(normalizedOutput)) {
    throw new Error(`Output path contains directory traversal: ${normalizedOutput}`);
  }

  // Output must stay inside the input directory. Comparing against
  // `inputDir + "/"` avoids the `/dir1` vs `/dir12` prefix collision.
  if (!normalizedOutput.startsWith(inputDir + "/")) {
    throw new Error(`Output path escapes input directory: ${normalizedOutput}`);
  }

  // MAX_PATH check. Local long-path inputs (`\\?\C:\...`,
  // forward-normalized to `//?/C:/...`) support up to 32767 chars on
  // Windows 10+. UNC long paths (`\\?\UNC\server\share\...` →
  // `//?/UNC/...`) keep the 260 cap because the server side may not
  // support long paths. Case-insensitive UNC prefix check so a
  // lowercased `//?/unc/...` still classifies as UNC.
  const lower = normalizedOutput.toLowerCase();
  const isLongLocalPath = lower.startsWith("//?/") && !lower.startsWith("//?/unc/");
  const maxPathLen = isLongLocalPath ? 32767 : 260;
  if (normalizedOutput.length > maxPathLen) {
    throw new Error(`Output path too long (${normalizedOutput.length} chars, max ${maxPathLen})`);
  }

  // Self-overwrite. Case-insensitive because Windows file names are
  // typically case-insensitive; conservative everywhere because the
  // app's primary platform is Windows.
  if (lower === normalizedInput.toLowerCase()) {
    throw new Error("Output path is the same as input (would overwrite source file)");
  }
}

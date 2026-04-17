/**
 * Output naming — resolve output file paths from templates.
 *
 * Port of Python output_naming.py. Handles template variables,
 * tag stripping, and safety checks (path traversal, reserved names).
 */
import type { Eotf } from "./color-engine";

// ── Template Presets ──────────────────────────────────────

export const OUTPUT_PRESETS = [
  "{name}.hdr.ass",
  "{name}.{eotf}.ass",
  "{name}.hdr.{eotf}.ass",
] as const;

export const DEFAULT_TEMPLATE = OUTPUT_PRESETS[0];

// ── Windows Reserved Names ────────────────────────────────
// These filenames are forbidden on Windows regardless of extension
const WINDOWS_RESERVED = new Set([
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
 * Resolve an output path from a template and input file path.
 *
 * @param inputPath - Full path to the input file
 * @param template - Output template string (e.g., "{name}.hdr.ass")
 * @param eotf - Transfer function for {eotf} variable
 * @returns Resolved output file path
 * @throws Error if template resolves to unsafe path
 */
export function resolveOutputPath(inputPath: string, template: string, eotf: Eotf): string {
  // Extract directory and base name from input path
  // Handle both forward and backslash separators
  const normalized = inputPath.replace(/\\/g, "/");
  const lastSlash = normalized.lastIndexOf("/");
  const dir = lastSlash >= 0 ? normalized.slice(0, lastSlash) : ".";
  if (dir === "." || dir === "") {
    throw new Error("Input path must be absolute");
  }
  const fullName = normalized.slice(lastSlash + 1);
  const dotIdx = fullName.lastIndexOf(".");
  let baseName = dotIdx > 0 ? fullName.slice(0, dotIdx) : fullName;

  // Strip existing .hdr / .sdr tags (any stacking order) to prevent doubling
  while (/\.(hdr|sdr)$/i.test(baseName)) {
    baseName = baseName.slice(0, -4);
  }

  // Guard: reject filenames with no valid stem (e.g., ".ass")
  if (!baseName || !baseName.replace(/^\.+/, "").trim()) {
    throw new Error("Input filename has no valid stem");
  }

  // Resolve template variables in a single pass to prevent double-substitution
  // (e.g., a filename containing literal "{eotf}" being expanded by the second replace)
  const resolved = template.replace(/\{(name|eotf)\}/g, (_, key: string) =>
    key === "name" ? baseName : eotf.toLowerCase()
  );

  // Safety: reject characters that are illegal in filenames on Windows
  const ILLEGAL_CHARS = /[<>:"|?*\\/]/;
  if (ILLEGAL_CHARS.test(resolved)) {
    throw new Error(`Output filename contains illegal characters: ${resolved}`);
  }

  // Safety: reject empty filename
  if (!resolved.trim()) {
    throw new Error("Template resolves to empty filename");
  }

  // Safety: check for Windows reserved names
  const stemDotIdx = resolved.lastIndexOf(".");
  const stem = (stemDotIdx > 0 ? resolved.slice(0, stemDotIdx) : resolved).replace(/[\s.]+$/, "");
  if (WINDOWS_RESERVED.has(stem.toUpperCase())) {
    throw new Error(`Output filename is a Windows reserved name: ${stem}`);
  }

  // Build full output path
  const outputPath = `${dir}/${resolved}`;

  // Safety: reject paths that exceed Windows MAX_PATH limit
  if (outputPath.length > 260) {
    throw new Error(
      `Output path too long (${outputPath.length} chars, max 260 for Windows compatibility)`
    );
  }

  // Safety: reject path traversal — check unconditionally
  // dir and outputPath are already forward-slash normalized (derived from `normalized`)
  if (/(^|\/)\.\.($|\/)/.test(outputPath)) {
    throw new Error(`Output path contains directory traversal: ${outputPath}`);
  }
  if (!outputPath.startsWith(dir + "/")) {
    throw new Error(`Output path escapes input directory: ${outputPath}`);
  }

  // Safety: output must have .ass extension
  if (!resolved.toLowerCase().endsWith(".ass")) {
    throw new Error("Output filename must end with .ass");
  }

  // Safety: reject self-overwrite (case-insensitive for Windows)
  if (outputPath.toLowerCase() === normalized.toLowerCase()) {
    throw new Error("Output path is the same as input (would overwrite source file)");
  }

  return outputPath;
}

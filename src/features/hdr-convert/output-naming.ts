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
  "CON", "PRN", "AUX", "NUL",
  "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
  "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
  "CONIN$", "CONOUT$",
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
export function resolveOutputPath(
  inputPath: string,
  template: string,
  eotf: Eotf
): string {
  // Extract directory and base name from input path
  // Handle both forward and backslash separators
  const normalized = inputPath.replace(/\\/g, "/");
  const lastSlash = normalized.lastIndexOf("/");
  const dir = lastSlash >= 0 ? normalized.slice(0, lastSlash) : ".";
  const fullName = normalized.slice(lastSlash + 1);
  const dotIdx = fullName.lastIndexOf(".");
  let baseName = dotIdx > 0 ? fullName.slice(0, dotIdx) : fullName;

  // Strip existing .hdr / .sdr tags to prevent doubling
  let changed = true;
  while (changed) {
    changed = false;
    for (const tag of [".hdr", ".sdr"]) {
      if (baseName.toLowerCase().endsWith(tag)) {
        baseName = baseName.slice(0, -tag.length);
        changed = true;
      }
    }
  }

  // Resolve template variables
  const resolved = template
    .replace(/\{name\}/g, baseName)
    .replace(/\{eotf\}/g, eotf.toLowerCase())
    .replace(/\{dir\}/g, dir);

  // Safety: reject empty filename
  if (!resolved.trim()) {
    throw new Error("Template resolves to empty filename");
  }

  // Safety: check for Windows reserved names
  const stem = resolved.slice(0, resolved.lastIndexOf(".")).replace(/[\s.]+$/, "");
  if (WINDOWS_RESERVED.has(stem.toUpperCase())) {
    throw new Error(`Output filename is a Windows reserved name: ${stem}`);
  }

  // Build full output path
  const outputPath = `${dir}/${resolved}`;

  // Safety: reject path traversal
  const normalizedOutput = outputPath.replace(/\\/g, "/");
  const normalizedDir = dir.replace(/\\/g, "/");
  if (!normalizedOutput.startsWith(normalizedDir + "/")) {
    // Could be a same-directory relative path, check again
    if (normalizedOutput.includes("..")) {
      throw new Error(`Output path escapes input directory: ${outputPath}`);
    }
  }

  // Safety: reject self-overwrite
  if (normalizedOutput === normalized) {
    throw new Error(
      "Output path is the same as input (would overwrite source file)"
    );
  }

  return outputPath;
}

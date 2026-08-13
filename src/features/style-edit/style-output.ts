import {
  assertSafeOutputFilename,
  assertSafeOutputPath,
  decomposeInputPath,
} from "../../lib/path-validation";

/**
 * Derive a safe sibling output for the style editor.
 *
 * The input's ASS/SSA extension and separator style are preserved. A prior
 * `.styled` infix is stripped before re-applying it; the shared self-overwrite
 * guard then rejects an already-styled input instead of creating a cumulative
 * name or overwriting the source.
 */
export function deriveStyledPath(inputPath: string): string {
  const parts = decomposeInputPath(inputPath);
  const { dir, ext, normalized, usedBackslash } = parts;
  let { baseName } = parts;

  const normalizedExtension = ext.toLowerCase();
  if (normalizedExtension !== ".ass" && normalizedExtension !== ".ssa") {
    throw new Error("Style editor input must use an .ass or .ssa extension");
  }
  if (baseName.toLowerCase().endsWith(".styled")) {
    baseName = baseName.slice(0, -".styled".length);
  }
  if (!baseName || !baseName.replace(/^\.+/, "").trim()) {
    throw new Error("Input filename has no valid stem after stripping .styled infix");
  }

  const outputName = `${baseName}.styled${ext}`;
  assertSafeOutputFilename(outputName);
  const outputPath = `${dir}/${outputName}`;
  assertSafeOutputPath(outputPath, normalized);
  return usedBackslash ? outputPath.replace(/\//g, "\\") : outputPath;
}

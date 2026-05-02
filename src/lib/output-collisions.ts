/**
 * Output-path collision detection.
 *
 * Feature tabs that auto-derive output paths from templates (HDR Convert
 * today; Tab 4 Batch Rename + Time Shift / Font Embed batch in later
 * stages) can silently overwrite previously-written files when the user
 * re-runs without realizing — they had no chance to opt in. The
 * antidote is a pre-flight pass that asks the OS whether each projected
 * output already exists; the consumer pairs the count with a confirm
 * dialog before entering its busy state.
 *
 * Save-As-style flows (single file picked through a native save dialog)
 * already have OS-level overwrite confirmation and don't need this util.
 */
import { exists } from "@tauri-apps/plugin-fs";

/**
 * Count how many of the given paths already exist on disk. Each check
 * runs as a single fs::stat in parallel, so batch latency is roughly
 * one round-trip regardless of the path count. Errors on individual
 * checks are treated as non-existent rather than propagated — a path
 * we can't stat is one we can't claim is colliding.
 */
export async function countExistingFiles(paths: string[]): Promise<number> {
  if (paths.length === 0) return 0;
  let errorCount = 0;
  const checks = await Promise.all(
    paths.map(async (p) => {
      try {
        return await exists(p);
      } catch {
        errorCount += 1;
        return false;
      }
    })
  );
  if (errorCount > 0) {
    console.warn(
      `[ssaHdrify] countExistingFiles ignored ${errorCount} stat failure(s); treating them as non-existent.`
    );
  }
  return checks.filter(Boolean).length;
}

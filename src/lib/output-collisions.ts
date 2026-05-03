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

/** Maximum concurrent fs::stat probes. Real-world batch sizes top out
 *  around 26 (typical anime episode count), so 32 covers the common
 *  case in one wave while bounding worst-case fan-out from a hostile-
 *  or-buggy-state caller supplying thousands of paths. Without this
 *  cap, Promise.all over 5000 paths would queue 5000 simultaneous IPC
 *  calls into Tauri's command pump and briefly hang the runtime. */
const MAX_CONCURRENT_STAT = 32;

/**
 * Count how many of the given paths already exist on disk. Stat checks
 * run in parallel (capped at `MAX_CONCURRENT_STAT`) so batch latency is
 * a small number of round-trips regardless of path count. Errors on
 * individual checks are treated as non-existent rather than propagated
 * — a path we can't stat is one we can't claim is colliding.
 */
export async function countExistingFiles(paths: string[]): Promise<number> {
  if (paths.length === 0) return 0;
  let errorCount = 0;
  let existingCount = 0;
  // Worker-pool pattern: keep up to MAX_CONCURRENT_STAT probes in flight;
  // each worker pulls the next index off a shared cursor. Order doesn't
  // matter for the count, so no result reassembly needed.
  let cursor = 0;
  const worker = async () => {
    while (true) {
      const idx = cursor++;
      if (idx >= paths.length) return;
      try {
        if (await exists(paths[idx])) {
          existingCount += 1;
        }
      } catch {
        errorCount += 1;
      }
    }
  };
  const workerCount = Math.min(MAX_CONCURRENT_STAT, paths.length);
  await Promise.all(Array.from({ length: workerCount }, () => worker()));
  if (errorCount > 0) {
    console.warn(
      `[ssaHdrify] countExistingFiles ignored ${errorCount} stat failure(s); treating them as non-existent.`
    );
  }
  return existingCount;
}

/**
 * Snapshot existing destinations before requesting overwrite consent.
 * Each writer uses that snapshot to choose replacement or exclusive creation.
 */
import { outputPathExists } from "./tauri-api";
import { normalizeOutputKey } from "./dedup-helpers";

/** Maximum concurrent fs::stat probes. Real-world batch sizes top out
 *  around 26 (typical anime episode count), so 32 covers the common
 *  case in one wave while bounding worst-case fan-out from a hostile-
 *  or-buggy-state caller supplying thousands of paths. Without this
 *  cap, Promise.all over 5000 paths would queue 5000 simultaneous IPC
 *  calls into Tauri's command pump and briefly hang the runtime. */
const MAX_CONCURRENT_STAT = 32;

/** Count unique existing destination keys; failed stat checks count as existing. */
export async function countExistingFiles(paths: string[]): Promise<number> {
  return (await findExistingOutputKeys(paths)).size;
}

/** Snapshot destinations covered by an overwrite prompt. Paths absent from
 * this snapshot must still use exclusive creation if they appear later. */
export async function findExistingOutputKeys(paths: string[]): Promise<ReadonlySet<string>> {
  const uniquePaths = [...new Map(paths.map((path) => [normalizeOutputKey(path), path])).values()];
  let errorCount = 0;
  const existingKeys = new Set<string>();
  // Worker-pool pattern: keep up to MAX_CONCURRENT_STAT probes in flight;
  // each worker pulls the next index off a shared cursor. Order doesn't
  // matter for the count, so no result reassembly needed.
  let cursor = 0;
  const worker = async () => {
    while (true) {
      const idx = cursor++;
      if (idx >= uniquePaths.length) return;
      const path = uniquePaths[idx]!;
      try {
        if (await outputPathExists(path)) {
          existingKeys.add(normalizeOutputKey(path));
        }
      } catch {
        // A failed probe must not silently authorize replacement.
        errorCount += 1;
        existingKeys.add(normalizeOutputKey(path));
      }
    }
  };
  const workerCount = Math.min(MAX_CONCURRENT_STAT, uniquePaths.length);
  await Promise.all(Array.from({ length: workerCount }, () => worker()));
  if (errorCount > 0) {
    console.warn(
      `[ssaHdrify] countExistingFiles: ${errorCount} stat failure(s) treated as existing (fail-safe overwrite-confirm bias).`
    );
  }
  return existingKeys;
}

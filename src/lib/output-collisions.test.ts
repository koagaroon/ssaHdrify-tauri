/**
 * Tests for countExistingFiles — the principle-#6 backstop that gates batch
 * writes behind an overwrite-confirmation prompt across all four tabs.
 *
 * Mocks @tauri-apps/plugin-fs so the suite runs in pure Node. The behaviors
 * pinned here are the ones the consumer tabs depend on: empty input is 0,
 * mixed inputs return the correct count, and a stat error counts as
 * non-existent (we never want a transient stat failure to block a save).
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

const existsMock = vi.fn();
vi.mock("@tauri-apps/plugin-fs", () => ({
  exists: (path: string) => existsMock(path),
}));

// Import AFTER vi.mock so the mocked module is picked up.
import { countExistingFiles } from "./output-collisions";

beforeEach(() => {
  existsMock.mockReset();
  vi.restoreAllMocks();
});

describe("countExistingFiles", () => {
  it("returns 0 for empty input without calling exists", async () => {
    const count = await countExistingFiles([]);
    expect(count).toBe(0);
    expect(existsMock).not.toHaveBeenCalled();
  });

  it("counts all paths when every one exists", async () => {
    existsMock.mockResolvedValue(true);
    const count = await countExistingFiles(["a.ass", "b.ass", "c.ass"]);
    expect(count).toBe(3);
    expect(existsMock).toHaveBeenCalledTimes(3);
  });

  it("returns 0 when no paths exist", async () => {
    existsMock.mockResolvedValue(false);
    const count = await countExistingFiles(["a.ass", "b.ass", "c.ass"]);
    expect(count).toBe(0);
    expect(existsMock).toHaveBeenCalledTimes(3);
  });

  it("returns the correct count for a mixed batch", async () => {
    existsMock.mockImplementation(async (p: string) => p === "a.ass" || p === "c.ass");
    const count = await countExistingFiles(["a.ass", "b.ass", "c.ass", "d.ass"]);
    expect(count).toBe(2);
  });

  it("treats a stat error as existing (fail-safe overwrite-confirm bias, Round 10 A-R10-013)", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    // First exists() throws, the rest report true. Round 10 A-R10-013
    // flipped the failure mode: stat errors now count as EXISTING
    // (count == 3, not 2), so the overwrite-confirm dialog fires
    // conservatively. Pre-R10 a transient EBUSY would suppress the
    // dialog and let the batch silently overwrite real files.
    existsMock.mockImplementationOnce(async () => {
      throw new Error("EBUSY");
    });
    existsMock.mockResolvedValue(true);
    const count = await countExistingFiles(["a.ass", "b.ass", "c.ass"]);
    expect(count).toBe(3);
    expect(warn).toHaveBeenCalledOnce();
    // Pin both the count AND the failure-mode wording. Pure noun-only
    // matching would miss a regression where errorCount interpolation
    // breaks; pure count-only matching would miss a wording flip away
    // from "stat failure/error" / "fail-safe".
    expect(warn.mock.calls[0][0]).toMatch(/\b1 stat (failure|error)/);
    expect(warn.mock.calls[0][0]).toMatch(/fail-safe|treated as existing/);
  });

  it("runs stat checks in parallel up to MAX_CONCURRENT_STAT", async () => {
    let inFlight = 0;
    let peakInFlight = 0;
    existsMock.mockImplementation(async () => {
      inFlight++;
      peakInFlight = Math.max(peakInFlight, inFlight);
      await new Promise((r) => setTimeout(r, 5));
      inFlight--;
      return true;
    });
    // 5 paths < MAX_CONCURRENT_STAT (32), so all 5 should run concurrently
    // in one wave. Anchor on equality, not >1 — a 2-wide chunked
    // regression would otherwise still pass the loose check.
    await countExistingFiles(["a", "b", "c", "d", "e"]);
    expect(peakInFlight).toBe(5);
  });

  it("caps concurrent stat checks at MAX_CONCURRENT_STAT for large input", async () => {
    let inFlight = 0;
    let peakInFlight = 0;
    existsMock.mockImplementation(async () => {
      inFlight++;
      peakInFlight = Math.max(peakInFlight, inFlight);
      await new Promise((r) => setTimeout(r, 1));
      inFlight--;
      return true;
    });
    // 100 paths > 32 cap. The new worker-pool pattern must cap
    // concurrency even though Promise.all in the original design
    // would have fired all 100 at once.
    const paths = Array.from({ length: 100 }, (_, i) => `p${i}`);
    await countExistingFiles(paths);
    expect(peakInFlight).toBeLessThanOrEqual(32);
    expect(peakInFlight).toBeGreaterThan(1);
  });
});

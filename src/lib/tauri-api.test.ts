/**
 * Tests for runStreamingScan (via the public scanFontDirectory wrapper).
 *
 * Mocks @tauri-apps/api/core to simulate Tauri's Channel<ScanProgress>:
 * the test drives the channel's onmessage handler manually, then resolves
 * the invoke promise. This pins the v1.3.1 streaming contract from the
 * frontend side — the integration test on the Rust side covers the
 * complementary half (channel-actually-streamed assertion).
 *
 * Two delivery patterns are exercised:
 *   1. Sync — batches fire synchronously while invoke is still running
 *      (small-payload `webview.eval` direct path). Accumulator is full
 *      when invoke resolves.
 *   2. Async-after-resolve — batches fire AFTER invoke resolves
 *      (large-payload `plugin:__TAURI_CHANNEL__|fetch` async path).
 *      runStreamingScan must still return the full set, because the
 *      Done sentinel arrives after every Batch and donePromise gates
 *      the return. This is the A-bug-1 regression test.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

interface MockChannel {
  onmessage: ((msg: unknown) => void) | null;
}

const channelInstances: MockChannel[] = [];
const invokeMock = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
  Channel: class {
    onmessage: ((msg: unknown) => void) | null = null;
    constructor() {
      channelInstances.push(this);
    }
  },
}));

// These two only need stub objects — runStreamingScan doesn't call them
// in this test, but tauri-api.ts imports them at top level.
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("@tauri-apps/plugin-fs", () => ({
  writeTextFile: vi.fn(),
  rename: vi.fn(),
  copyFile: vi.fn(),
}));

// Import AFTER vi.mock so the mocked Channel is picked up.
import { scanFontDirectory } from "./tauri-api";

beforeEach(() => {
  channelInstances.length = 0;
  invokeMock.mockReset();
});

function makeRawEntry(name: string) {
  return {
    path: `C:/Fonts/${name}.ttf`,
    index: 0,
    families: [name],
    bold: false,
    italic: false,
    size_bytes: 1234,
  };
}

describe("runStreamingScan — sync delivery (batches arrive during invoke)", () => {
  it("invokes onBatch per batch with monotonically increasing totals", async () => {
    invokeMock.mockImplementation(async () => {
      const channel = channelInstances[0];
      channel.onmessage?.({ kind: "batch", entries: [makeRawEntry("A"), makeRawEntry("B")] });
      channel.onmessage?.({ kind: "batch", entries: [makeRawEntry("C")] });
      channel.onmessage?.({ kind: "done" });
    });

    const seenTotals: number[] = [];
    const seenDeltaSizes: number[] = [];
    const result = await scanFontDirectory("/fake/dir", (delta, total) => {
      seenDeltaSizes.push(delta.length);
      seenTotals.push(total);
    });

    expect(result.length).toBe(3);
    expect(result.map((e) => e.families[0])).toEqual(["A", "B", "C"]);
    expect(seenDeltaSizes).toEqual([2, 1]);
    expect(seenTotals).toEqual([2, 3]);
  });

  it("returns an empty array when no batches arrive (Done only)", async () => {
    invokeMock.mockImplementation(async () => {
      channelInstances[0].onmessage?.({ kind: "done" });
    });

    const result = await scanFontDirectory("/empty/dir");
    expect(result).toEqual([]);
  });
});

describe("runStreamingScan — async-after-resolve (A-bug-1 regression)", () => {
  it("waits for Done before returning, even when batches fire after invoke resolves", async () => {
    // Simulate Tauri's large-payload fetch path: invoke promise resolves
    // BEFORE any batch lands. Without the Done-sentinel guard,
    // runStreamingScan would return an empty accumulator here.
    invokeMock.mockImplementation(async () => {
      // Schedule batches + Done as separate microtasks AFTER this returns.
      queueMicrotask(() => {
        channelInstances[0].onmessage?.({
          kind: "batch",
          entries: [makeRawEntry("X"), makeRawEntry("Y")],
        });
      });
      queueMicrotask(() => {
        channelInstances[0].onmessage?.({ kind: "batch", entries: [makeRawEntry("Z")] });
      });
      queueMicrotask(() => {
        channelInstances[0].onmessage?.({ kind: "done" });
      });
    });

    const result = await scanFontDirectory("/large/dir");
    // If runStreamingScan returned at `await invoke`, this would be 0.
    // The Done sentinel + donePromise guarantee we wait for the full set.
    expect(result.length).toBe(3);
    expect(result.map((e) => e.families[0])).toEqual(["X", "Y", "Z"]);
  });
});

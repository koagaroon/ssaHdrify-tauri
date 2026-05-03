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
 *      (small-payload `webview.eval` direct path). Progress counts arrive
 *      before invoke resolves.
 *   2. Async-after-resolve — batches fire AFTER invoke resolves
 *      (large-payload `plugin:__TAURI_CHANNEL__|fetch` async path).
 *      runStreamingScan must still return final counts, because the
 *      Done sentinel arrives after every Batch and donePromise gates
 *      the return. This is the A-bug-1 regression test.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

interface MockChannel {
  onmessage: ((msg: unknown) => void) | null;
}

const channelInstances: MockChannel[] = [];
const invokeMock = vi.fn();
const openMock = vi.fn();

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
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: (...args: unknown[]) => openMock(...args) }));
vi.mock("@tauri-apps/plugin-fs", () => ({
  writeTextFile: vi.fn(),
  rename: vi.fn(),
  copyFile: vi.fn(),
}));

// Import AFTER vi.mock so the mocked Channel is picked up.
import {
  pickAssFiles,
  pickFontFiles,
  pickRenameInputs,
  preflightFontDirectory,
  preflightFontFiles,
  scanFontDirectory,
  scanFontFiles,
} from "./tauri-api";

beforeEach(() => {
  channelInstances.length = 0;
  invokeMock.mockReset();
  openMock.mockReset();
});

describe("runStreamingScan — sync delivery (batches arrive during invoke)", () => {
  it("invokes onBatch per batch with monotonically increasing totals", async () => {
    invokeMock.mockImplementation(async () => {
      const channel = channelInstances[0];
      channel.onmessage?.({ kind: "batch", total: 2 });
      channel.onmessage?.({ kind: "batch", total: 3 });
      channel.onmessage?.({
        kind: "done",
        reason: "natural",
        added: 3,
        duplicated: 1,
      });
    });

    const seenTotals: number[] = [];
    const result = await scanFontDirectory("/fake/dir", "source-a", 101, (total) => {
      seenTotals.push(total);
    });

    // Pin every field of FontScanResult — including `reason`. toEqual
    // accepts missing keys silently, so omitting reason from either
    // the mocked Done OR this assertion would let a regression where
    // runStreamingScan dropped the field assignment slip past.
    expect(result).toEqual({ added: 3, duplicated: 1, reason: "natural" });
    expect(seenTotals).toEqual([2, 3]);
    expect(invokeMock).toHaveBeenCalledWith(
      "scan_font_directory",
      expect.objectContaining({ dir: "/fake/dir", sourceId: "source-a", scanId: 101 })
    );
  });

  it("returns zero counts when no batches arrive (Done only)", async () => {
    invokeMock.mockImplementation(async () => {
      channelInstances[0].onmessage?.({
        kind: "done",
        reason: "natural",
        added: 0,
        duplicated: 0,
      });
    });

    const result = await scanFontDirectory("/empty/dir", "source-empty", 102);
    expect(result).toEqual({ added: 0, duplicated: 0, reason: "natural" });
  });

  it("propagates reason='ceilingHit' on MAX_FONTS_PER_SCAN stop", async () => {
    // Documented Rust wire contract: a defense-ceiling stop produces
    // `reason: "ceilingHit"` (single state, no flag-pair impossible-
    // combination foot-gun). Pin this so a future Rust-side rename of
    // the variant or a runStreamingScan field-drop is caught.
    invokeMock.mockImplementation(async () => {
      channelInstances[0].onmessage?.({
        kind: "done",
        reason: "ceilingHit",
        added: 100_000,
        duplicated: 0,
      });
    });

    const result = await scanFontDirectory("/huge/dir", "source-huge", 105);
    expect(result).toEqual({
      added: 100_000,
      duplicated: 0,
      reason: "ceilingHit",
    });
  });
});

describe("runStreamingScan via scanFontFiles", () => {
  it("forwards the path list and resolves with batched counts", async () => {
    invokeMock.mockImplementation(async () => {
      channelInstances[0].onmessage?.({ kind: "batch", total: 1 });
      channelInstances[0].onmessage?.({ kind: "batch", total: 2 });
      channelInstances[0].onmessage?.({
        kind: "done",
        reason: "natural",
        added: 2,
        duplicated: 0,
      });
    });

    const seen: number[] = [];
    const result = await scanFontFiles(
      ["D:/A.ttf", "D:/B.otf"],
      "source-files",
      201,
      (total) => seen.push(total)
    );

    expect(result).toEqual({ added: 2, duplicated: 0, reason: "natural" });
    expect(seen).toEqual([1, 2]);
    // Pin the exact arg shape so a future runStreamingScan refactor
    // that drops or renames a field is caught (matches the dual cap
    // boundary covered on the Rust side in test_scan).
    expect(invokeMock).toHaveBeenCalledWith("scan_font_files", {
      paths: ["D:/A.ttf", "D:/B.otf"],
      sourceId: "source-files",
      scanId: 201,
      progress: channelInstances[0],
    });
  });

  it("forwards a 1000-element path list at the boundary", async () => {
    const paths = Array.from({ length: 1000 }, (_, i) => `D:/dummy-${i}.ttf`);
    invokeMock.mockImplementation(async () => {
      channelInstances[0].onmessage?.({
        kind: "done",
        reason: "natural",
        added: 0,
        duplicated: 0,
      });
    });
    const result = await scanFontFiles(paths, "source-boundary", 202);
    expect(result.added).toBe(0);
    // The frontend passes the slice through unchanged; the MAX_INPUT_PATHS
    // gate lives on the Rust side. Pin that we don't truncate or re-sort.
    expect(invokeMock).toHaveBeenCalledWith(
      "scan_font_files",
      expect.objectContaining({ paths, sourceId: "source-boundary", scanId: 202 })
    );
  });

  it("clears the batch count when invoke rejects", async () => {
    invokeMock.mockImplementation(async () => {
      channelInstances[0].onmessage?.({ kind: "batch", total: 5 });
      throw new Error("SQLite BUSY: database is locked");
    });

    const seen: number[] = [];
    await expect(
      scanFontFiles(["D:/A.ttf"], "source-rollback", 203, (total) => seen.push(total))
    ).rejects.toThrow(/SQLite BUSY/);
    // Provisional 5 first, then reset to 0 in the catch path. The reset
    // is what keeps "Scanned 5 fonts" out of a UI panel that's about to
    // surface the rollback error.
    expect(seen).toEqual([5, 0]);
  });
});

describe("font scan preflight wrappers", () => {
  it("invokes the directory and file-list preflight commands with stable args", async () => {
    invokeMock.mockResolvedValueOnce({ fontFiles: 12, totalBytes: 34 });
    await expect(preflightFontDirectory("D:/Fonts")).resolves.toEqual({
      fontFiles: 12,
      totalBytes: 34,
    });
    expect(invokeMock).toHaveBeenLastCalledWith("preflight_font_directory", {
      dir: "D:/Fonts",
    });

    invokeMock.mockResolvedValueOnce({ fontFiles: 2, totalBytes: 56 });
    await expect(preflightFontFiles(["D:/A.ttf", "D:/B.otf"])).resolves.toEqual({
      fontFiles: 2,
      totalBytes: 56,
    });
    expect(invokeMock).toHaveBeenLastCalledWith("preflight_font_files", {
      paths: ["D:/A.ttf", "D:/B.otf"],
    });
  });
});

describe("localized native file dialogs", () => {
  const zh = (key: string): string =>
    ({
      dialog_filter_all_files: "所有文件",
      dialog_filter_ass_ssa_subtitles: "ASS/SSA 字幕",
      dialog_filter_font_files: "字体文件",
      dialog_filter_subtitle_files: "字幕文件",
      dialog_filter_video_files: "视频文件",
      dialog_filter_video_subtitle_files: "视频和字幕文件",
      dialog_pick_ass_files_title: "选择 ASS/SSA 文件",
      dialog_pick_font_files_title: "选择字体文件",
      dialog_pick_rename_inputs_title: "选择视频和字幕",
    })[key] ?? key;

  it("uses translated titles and filters for font files", async () => {
    openMock.mockResolvedValue(["D:/Fonts/A.ttf"]);
    await pickFontFiles(zh);

    expect(openMock).toHaveBeenCalledWith({
      multiple: true,
      title: "选择字体文件",
      filters: [
        { name: "字体文件", extensions: ["ttf", "otf", "ttc", "otc"] },
        { name: "所有文件", extensions: ["*"] },
      ],
    });
  });

  it("uses ASS/SSA wording for ASS picker title and filter", async () => {
    openMock.mockResolvedValue(["D:/Subs/A.ass"]);
    await pickAssFiles(zh);

    expect(openMock).toHaveBeenCalledWith({
      multiple: true,
      title: "选择 ASS/SSA 文件",
      filters: [
        { name: "ASS/SSA 字幕", extensions: ["ass", "ssa"] },
        { name: "所有文件", extensions: ["*"] },
      ],
    });
  });

  it("keeps ASS/SSA wording in fallback picker labels", async () => {
    openMock.mockResolvedValue(["D:/Subs/A.ssa"]);
    await pickAssFiles();

    expect(openMock).toHaveBeenCalledWith({
      multiple: true,
      title: "Select ASS/SSA files",
      filters: [
        { name: "ASS/SSA Subtitles", extensions: ["ass", "ssa"] },
        { name: "All Files", extensions: ["*"] },
      ],
    });
  });

  it("uses translated titles and filters for mixed rename inputs", async () => {
    openMock.mockResolvedValue(["D:/Show.mkv", "D:/Show.ass"]);
    await pickRenameInputs(zh);

    expect(openMock).toHaveBeenCalledWith(
      expect.objectContaining({
        multiple: true,
        title: "选择视频和字幕",
        filters: expect.arrayContaining([
          expect.objectContaining({ name: "视频和字幕文件" }),
          expect.objectContaining({ name: "视频文件" }),
          expect.objectContaining({ name: "字幕文件" }),
          expect.objectContaining({ name: "所有文件" }),
        ]),
      })
    );
  });
});

describe("runStreamingScan — async-after-resolve (A-bug-1 regression)", () => {
  it("waits for Done before returning, even when batches fire after invoke resolves", async () => {
    // Simulate Tauri's large-payload fetch path: invoke promise resolves
    // BEFORE any batch lands. Without the Done-sentinel guard,
    // runStreamingScan would return zero counts here.
    invokeMock.mockImplementation(async () => {
      // Schedule batches + Done as separate microtasks AFTER this returns.
      queueMicrotask(() => {
        channelInstances[0].onmessage?.({ kind: "batch", total: 2 });
      });
      queueMicrotask(() => {
        channelInstances[0].onmessage?.({ kind: "batch", total: 3 });
      });
      queueMicrotask(() => {
        channelInstances[0].onmessage?.({
          kind: "done",
          reason: "natural",
          added: 3,
          duplicated: 0,
        });
      });
    });

    const result = await scanFontDirectory("/large/dir", "source-large", 103);
    // If runStreamingScan returned at `await invoke`, this would still be 0.
    // The Done sentinel + donePromise guarantee we wait for the full set.
    expect(result.added).toBe(3);
    expect(result.reason).toBe("natural");
  });

  it("returns the Rust-reported cancellation outcome after async Done", async () => {
    invokeMock.mockImplementation(async () => {
      queueMicrotask(() => {
        channelInstances[0].onmessage?.({ kind: "batch", total: 40 });
      });
      queueMicrotask(() => {
        channelInstances[0].onmessage?.({
          kind: "done",
          reason: "userCancel",
          added: 40,
          duplicated: 0,
        });
      });
    });

    const result = await scanFontDirectory("/cancelled/dir", "source-cancelled", 104);
    expect(result).toEqual({ added: 40, duplicated: 0, reason: "userCancel" });
  });
});

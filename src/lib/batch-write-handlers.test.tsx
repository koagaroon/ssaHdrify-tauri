/// <reference types="node" />
import { resolve } from "node:path";
import { readFileSync } from "node:fs";
import { isValidElement, type ReactElement, type ReactNode } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import TimingShift from "../features/timing-shift/TimingShift";
import HdrConvert from "../features/hdr-convert/HdrConvert";
import FontEmbed from "../features/font-embed/FontEmbed";
import BatchRename from "../features/batch-rename/BatchRename";
import {
  useFileContext,
  type FontsFilesState,
  type BatchRenameFilesState,
  type HdrFileState,
  type TimingFilesState,
  type StyleFilesState,
} from "./FileContext";
import { SELECTED_INPUT_CONFLICT_BATCH_LIMIT } from "./tauri-api";

const state = vi.hoisted(() => ({
  values: [] as unknown[],
  cursor: 0,
  paths: [] as string[],
  files: new Map<string, string>(),
  attempts: [] as { path: string; overwrite: boolean }[],
  probe: undefined as undefined | ((path: string) => Promise<boolean>),
  conflictProbe: undefined as undefined | (() => Promise<string[]>),
  aliasConflicts: [] as string[],
  aliases: new Map<string, string>(),
  conflictCalls: [] as { inputPaths: string[]; outputPaths: string[] }[],
  readProbe: undefined as undefined | ((path: string) => Promise<void>),
  onRead: undefined as undefined | ((path: string) => void),
  onWrite: undefined as undefined | ((path: string) => void),
  onDrop: undefined as undefined | ((paths: string[]) => Promise<void>),
  effects: [] as (() => unknown)[],
  fontsFiles: null as FontsFilesState | null,
  renameFiles: null as BatchRenameFilesState | null,
  hdrFiles: undefined as HdrFileState | null | undefined,
  timingFiles: undefined as TimingFilesState | null | undefined,
  styleFiles: null as StyleFilesState | null,
  setHdrFiles: vi.fn(),
  setTimingFiles: vi.fn(),
  ask: vi.fn(),
  open: vi.fn(),
  log: vi.fn(),
}));

// Run the real component handlers and file adapters with explicit rerenders.
// Browser effects/layout and the native filesystem are separate test boundaries.
vi.mock("react", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react")>()),
  useState: (initial: unknown) => {
    const slot = state.cursor++;
    if (!(slot in state.values))
      state.values[slot] = typeof initial === "function" ? initial() : initial;
    return [
      state.values[slot],
      (next: unknown) => {
        state.values[slot] = typeof next === "function" ? next(state.values[slot]) : next;
      },
    ];
  },
  useRef: (initial: unknown) => {
    const slot = state.cursor++;
    if (!(slot in state.values)) state.values[slot] = { current: initial };
    return state.values[slot];
  },
  useMemo: (compute: () => unknown) => compute(),
  useCallback: (callback: unknown) => callback,
  useEffect: (effect: () => unknown) => {
    state.effects.push(effect);
  },
}));
vi.mock("@tauri-apps/api/core", () => ({
  Channel: class {},
  invoke: async (command: string, args: Record<string, unknown>) => {
    const path = String(args.path);
    if (command === "safe_find_selected_input_conflicts") {
      const inputPaths = args.inputPaths as string[];
      const outputPaths = args.outputPaths as string[];
      state.conflictCalls.push({ inputPaths, outputPaths });
      if (inputPaths.length > 5000 || outputPaths.length > 5000)
        throw new Error("Native batch limit");
      if (state.conflictProbe) return state.conflictProbe();
      const canonical = (value: string) => state.aliases.get(value) ?? value;
      const selected = new Set(inputPaths.map(canonical));
      return outputPaths.filter(
        (output) => selected.has(canonical(output)) || state.aliasConflicts.includes(output)
      );
    }
    if (command === "safe_output_path_exists") {
      return state.probe ? state.probe(path) : state.files.has(path);
    }
    if (command === "read_text_detect_encoding") {
      await state.readProbe?.(path);
      state.onRead?.(path);
      const text = state.files.get(path);
      if (text === undefined) throw new Error("Missing input");
      return { text, encoding: "UTF-8", encodingId: "utf-8", inferredWithoutBom: false };
    }
    if (command === "safe_write_text_file") {
      state.onWrite?.(path);
      state.attempts.push({ path, overwrite: args.overwrite === true });
      if (state.files.has(path) && !args.overwrite) throw new Error("Destination already exists");
      state.files.set(path, String(args.content));
      return;
    }
    if (command === "safe_copy_file" || command === "safe_rename_file") {
      const destination = String(args.dst);
      state.onWrite?.(destination);
      state.attempts.push({ path: destination, overwrite: args.overwrite === true });
      if (state.files.has(destination) && !args.overwrite)
        throw new Error("Destination already exists");
      const source = String(args.src);
      const content = state.files.get(source);
      if (content === undefined) throw new Error("Missing input");
      state.files.set(destination, content);
      if (command === "safe_rename_file") state.files.delete(source);
      return;
    }
    if (command === "resolve_user_font" || command === "lookup_font_family") return null;
    if (command === "find_system_font") return { path: "/fonts/fixture.ttf", index: 0 };
    if (command === "subset_font_bytes") return new Uint8Array([1, 2, 3]).buffer;
    throw new Error(`Unexpected native command: ${command}`);
  },
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  ask: (...args: unknown[]) => state.ask(...args),
  open: (...args: unknown[]) => state.open(...args),
}));
vi.mock("../i18n/useI18n", () => ({ useI18n: () => ({ t: (key: string) => key }) }));
vi.mock("./FileContext", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./FileContext")>();
  return {
    ...actual,
    useFileContext: () => {
      const selection = {
        filePaths: state.paths,
        fileNames: state.paths.map((path) => path.split("/").at(-1)!),
        firstFileContent: "",
      };
      // Exercise the real provider's ownership snapshot and batching method;
      // isolate its hook slots from the feature component being rendered.
      const savedValues = state.values;
      const savedCursor = state.cursor;
      let context: ReturnType<typeof actual.useFileContext>;
      try {
        state.values = [
          state.hdrFiles === undefined ? selection : state.hdrFiles,
          state.timingFiles === undefined ? selection : state.timingFiles,
          state.fontsFiles,
          state.renameFiles,
          state.styleFiles,
        ];
        state.cursor = 0;
        context = actual.FileProvider({ children: null }).props.value;
      } finally {
        state.values = savedValues;
        state.cursor = savedCursor;
      }
      return {
        ...context,
        setFontsFiles: (value: FontsFilesState | null) => {
          state.fontsFiles = value;
        },
        setRenameFiles: (value: BatchRenameFilesState | null) => {
          state.renameFiles = value;
        },
        clearFile: vi.fn(),
        setTimingFiles: state.setTimingFiles,
        setHdrFiles: state.setHdrFiles,
      };
    },
  };
});
vi.mock("./useFolderDrop", () => ({
  useFolderDrop: (options: { onPaths: typeof state.onDrop }) => {
    state.onDrop = options.onPaths;
  },
}));
vi.mock("./useClickOutside", () => ({ useClickOutside: () => {} }));
vi.mock("./useTabStatus", () => ({ useTabStatus: () => {} }));
vi.mock("./useLogPanel", () => ({
  useLogPanel: () => ({
    logs: [],
    addLog: state.log,
    clearLogs: vi.fn(),
    logScrollRef: { current: null },
  }),
}));

type Props = {
  children?: ReactNode;
  title?: string;
  disabled?: boolean;
  value?: string;
  onClick?: () => unknown;
  onChange?: () => unknown;
};
function elements(node: ReactNode): ReactElement<Props>[] {
  if (Array.isArray(node)) return node.flatMap(elements);
  if (!isValidElement<Props>(node)) return [];
  return [node, ...elements(node.props.children)];
}
const subtitle = "1\n00:00:01,000 --> 00:00:02,000\nhello\n";
const inputPath = (name: string) => resolve("handler-fixtures", name).replaceAll("\\", "/");

beforeEach(() => {
  state.values = [];
  state.cursor = 0;
  state.paths = [inputPath("episode.srt")];
  state.files = new Map([[state.paths[0]!, subtitle]]);
  state.attempts = [];
  state.probe = undefined;
  state.conflictProbe = undefined;
  state.aliasConflicts = [];
  state.onRead = undefined;
  state.readProbe = undefined;
  state.aliases = new Map();
  state.conflictCalls = [];
  state.onWrite = undefined;
  state.onDrop = undefined;
  state.effects = [];
  state.fontsFiles = null;
  state.renameFiles = null;
  state.hdrFiles = undefined;
  state.timingFiles = undefined;
  state.styleFiles = null;
  state.setHdrFiles.mockReset().mockImplementation((value: HdrFileState | null) => {
    state.hdrFiles = value;
  });
  state.setTimingFiles.mockReset().mockImplementation((value: TimingFilesState | null) => {
    state.timingFiles = value;
  });
  state.ask.mockReset().mockResolvedValue(true);
  state.open.mockReset().mockResolvedValue(null);
  state.log.mockReset();
});

const assSubtitle = `[Script Info]
ScriptType: v4.00+
[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,Fixture,20,0,0
[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:01.00,0:00:02.00,Default,Hello
`;

describe("loaded subtitle protection", () => {
  it("keeps the shared IPC chunk limit equal to the native limit", () => {
    const source = readFileSync(resolve("src-tauri/src/dropzone.rs"), "utf8");
    const nativeLimit = Number(source.match(/MAX_RESULT_FILES: usize = (\d+);/)?.[1]);
    expect(SELECTED_INPUT_CONFLICT_BATCH_LIMIT).toBe(nativeLimit);
  });

  it("includes every tab's subtitle paths and excludes rename videos", async () => {
    const names = ["hdr.srt", "timing.vtt", "fonts.ass", "sidecar.sup", "style.ssa"];
    const paths = names.map(inputPath);
    state.hdrFiles = { filePaths: [paths[0]!], fileNames: [names[0]!] };
    state.timingFiles = { filePaths: [paths[1]!], fileNames: [names[1]!], firstFileContent: "" };
    state.fontsFiles = { filePaths: [paths[2]!], fileNames: [names[2]!], firstFileContent: "" };
    state.renameFiles = {
      subtitlePaths: [paths[3]!],
      subtitleNames: [names[3]!],
      videoPaths: [inputPath("video.mkv")],
      videoNames: ["video.mkv"],
    };
    state.styleFiles = { filePaths: [paths[4]!], fileNames: [names[4]!] };
    await useFileContext().findLoadedInputConflictKeys(paths);
    expect(state.conflictCalls).toEqual([{ inputPaths: paths, outputPaths: paths }]);
  });

  it("deduplicates and batches more than 5000 combined inputs and outputs without dropping the last source", async () => {
    const paths = Array.from({ length: 5001 }, (_, index) => inputPath(`source-${index}.ass`));
    state.hdrFiles = { filePaths: paths.slice(0, 5000), fileNames: [] };
    state.timingFiles = {
      filePaths: [paths[0]!, paths[5000]!],
      fileNames: [],
      firstFileContent: "",
    };
    const result = await useFileContext().findLoadedInputConflictKeys([...paths, paths[0]!]);
    expect(result.size).toBe(5001);
    expect(
      state.conflictCalls.map((call) => [call.inputPaths.length, call.outputPaths.length])
    ).toEqual([
      [5000, 5000],
      [1, 5000],
      [5000, 1],
      [1, 1],
    ]);
  });

  it("stops checking further chunks after cancellation", async () => {
    state.hdrFiles = {
      filePaths: Array.from({ length: 5001 }, (_, index) => inputPath(`source-${index}.ass`)),
      fileNames: [],
    };
    state.timingFiles = null;
    const controller = new AbortController();
    state.conflictProbe = async () => {
      controller.abort();
      return [];
    };
    await useFileContext().findLoadedInputConflictKeys(
      [inputPath("output.ass")],
      controller.signal
    );
    expect(state.conflictCalls).toHaveLength(1);
  });
});

describe.each(["Font Embed", "Batch Copy", "Batch Rename"] as const)(
  "%s write preparation",
  (kind) => {
    const isFont = kind === "Font Embed";
    const isRename = kind === "Batch Rename";
    const button = isFont ? "btn_embed" : "btn_rename_run";
    const source = (number = 1) => inputPath(`sub EP${number.toString().padStart(2, "0")}.ass`);
    const output = (number = 1) =>
      inputPath(
        isFont
          ? `sub EP${number.toString().padStart(2, "0")}.embedded.ass`
          : `video EP${number.toString().padStart(2, "0")}.ass`
      );

    function render() {
      state.cursor = 0;
      state.effects = [];
      return elements(
        isFont ? FontEmbed({ cacheStatus: null, cacheProbeFailed: false }) : BatchRename()
      );
    }
    function labeled(label: string) {
      const match = render().find(
        (entry) => entry.type === "button" && entry.props.children === label
      );
      expect(match, label).toBeDefined();
      return match!.props;
    }
    async function prepare(count = 1, extraInput?: string) {
      const paths = Array.from({ length: count }, (_, index) => source(index + 1));
      if (extraInput) paths.push(extraInput);
      for (const path of paths) state.files.set(path, assSubtitle);
      if (isFont) {
        render();
        await state.onDrop!(paths);
      } else {
        state.renameFiles = {
          subtitlePaths: paths,
          subtitleNames: paths.map((path) => path.split("/").at(-1)!),
          videoPaths: Array.from({ length: count }, (_, index) =>
            inputPath(`video EP${(index + 1).toString().padStart(2, "0")}.mkv`)
          ),
          videoNames: Array.from(
            { length: count },
            (_, index) => `video EP${(index + 1).toString().padStart(2, "0")}.mkv`
          ),
        };
        render();
        // Seed the real pairing state with its mount effects; subsequent renders
        // keep the manual state, as React does while the selection is unchanged.
        for (const effect of state.effects) effect();
        if (isRename)
          render().find((entry) => entry.type === "input" && entry.props.value === "rename")!.props
            .onChange!();
      }
      expect(labeled(button).disabled).toBe(false);
    }

    it("creates the predicted output and preserves or moves the source as selected", async () => {
      await prepare();
      await labeled(button).onClick!();
      expect(state.attempts).toEqual([{ path: output(), overwrite: false }]);
      expect(state.files.get(output())).toContain("Hello");
      expect(state.files.get(source())).toBe(isRename ? undefined : assSubtitle);
      expect(state.ask).toHaveBeenCalledTimes(isRename ? 1 : 0);
    });

    it("only grants replacement to the existing output covered by acceptance", async () => {
      await prepare(2);
      state.files.set(output(), "approved old output");
      state.onWrite = (path) => {
        if (path === output()) state.files.set(output(2), "late file");
      };
      await labeled(button).onClick!();
      expect(state.ask).toHaveBeenCalledTimes(isRename ? 2 : 1);
      expect(state.attempts).toEqual([
        { path: output(), overwrite: true },
        { path: output(2), overwrite: false },
      ]);
      expect(state.files.get(output())).toContain("Hello");
      expect(state.files.get(output(2))).toBe("late file");
      expect(state.files.get(source(2))).toBe(assSubtitle);
    });

    it.each([false, true])(
      "protects a source loaded in Style Edit (directory alias=%s)",
      async (alias) => {
        await prepare();
        const name = output().split("/").at(-1)!;
        const selected = alias ? inputPath(`style-alias/${name}`) : output();
        if (alias) state.aliases.set(output(), selected);
        state.styleFiles = { filePaths: [selected], fileNames: [name] };
        state.files.set(output(), assSubtitle);
        await labeled(button).onClick!();
        expect(state.attempts).toEqual([]);
        expect(state.files.get(output())).toBe(assSubtitle);
        expect(state.files.get(source())).toBe(assSubtitle);
        expect(state.ask).not.toHaveBeenCalled();
        state.styleFiles = null;
        await labeled(button).onClick!();
        expect(state.attempts).toEqual([{ path: output(), overwrite: true }]);
      }
    );

    it("keeps selected source bytes even when they match a projected destination", async () => {
      await prepare(1, output());
      await labeled(button).onClick!();
      expect(state.files.get(source())).toBe(assSubtitle);
      expect(state.files.get(output())).toBe(assSubtitle);
      expect(
        state.attempts.some((attempt) => attempt.path === source() || attempt.path === output())
      ).toBe(false);
      expect(state.ask).not.toHaveBeenCalled();
    });

    it("declining overwrite preserves the destination and returns to idle", async () => {
      await prepare();
      state.files.set(output(), "previous output");
      if (isRename) state.ask.mockResolvedValueOnce(true);
      state.ask.mockResolvedValue(false);
      await labeled(button).onClick!();
      expect(state.attempts).toEqual([]);
      expect(state.files.get(output())).toBe("previous output");
      expect(state.files.get(source())).toBe(assSubtitle);
      expect(labeled(button).disabled).toBe(false);
    });

    it("blocks an existing alias of a selected source before asking about overwrite", async () => {
      await prepare();
      state.files.set(output(), assSubtitle);
      state.aliasConflicts = [output()];
      await labeled(button).onClick!();
      expect(state.attempts).toEqual([]);
      expect(state.ask).not.toHaveBeenCalled();
      expect(state.files.get(source())).toBe(assSubtitle);
      expect(state.files.get(output())).toBe(assSubtitle);
      expect(labeled(button).disabled).toBe(false);
    });

    it("keeps unrelated batch outputs when one output aliases a selected source", async () => {
      await prepare(2);
      state.files.set(output(), assSubtitle);
      state.aliasConflicts = [output()];
      await labeled(button).onClick!();
      expect(state.attempts).toEqual([{ path: output(2), overwrite: false }]);
      expect(state.ask).toHaveBeenCalledTimes(isRename ? 1 : 0);
      expect(state.files.get(source())).toBe(assSubtitle);
      expect(state.files.get(output())).toBe(assSubtitle);
      expect(state.files.get(output(2))).toContain("Hello");
    });

    it("shows busy controls before delayed preparation and cancels without a write", async () => {
      await prepare();
      let finishProbe!: () => void;
      state.conflictProbe = () =>
        new Promise<string[]>((resolveProbe) => {
          finishProbe = () => resolveProbe([]);
        });
      const pending = labeled(button).onClick!();
      expect(render().find((entry) => entry.props.title === "btn_clear_file")?.props.disabled).toBe(
        true
      );
      labeled("btn_cancel").onClick!();
      finishProbe();
      await pending;
      expect(state.attempts).toEqual([]);
      expect(state.ask).not.toHaveBeenCalled();
      expect(labeled(button).disabled).toBe(false);
    });

    it("honors Cancel while confirmation is pending even if the dialog later accepts", async () => {
      await prepare();
      state.files.set(output(), "previous output");
      let finishAsk: undefined | (() => void);
      state.ask.mockImplementation(
        () =>
          new Promise<boolean>((resolveAsk) => {
            finishAsk = () => resolveAsk(true);
          })
      );
      const pending = labeled(button).onClick!();
      await vi.waitFor(() => expect(finishAsk).toBeTypeOf("function"));
      expect(render().find((entry) => entry.props.title === "btn_clear_file")?.props.disabled).toBe(
        true
      );
      labeled("btn_cancel").onClick!();
      finishAsk!();
      await pending;
      expect(state.attempts).toEqual([]);
      expect(state.files.get(output())).toBe("previous output");
      expect(state.files.get(source())).toBe(assSubtitle);
      expect(labeled(button).disabled).toBe(false);
    });

    it("ignores a file picker that completes after the current batch starts", async () => {
      await prepare();
      const originalSelection = isFont ? state.fontsFiles : state.renameFiles;
      let finishPick!: (paths: string[]) => void;
      state.open.mockImplementation(
        () =>
          new Promise<string[]>((resolvePick) => {
            finishPick = resolvePick;
          })
      );
      const pendingPick = labeled(isFont ? "btn_select_files" : "btn_select_rename_inputs")
        .onClick!();
      await labeled(button).onClick!();
      finishPick([inputPath("later EP09.ass")]);
      await pendingPick;
      if (isFont) expect(state.fontsFiles).toBe(originalSelection);
      else expect(state.renameFiles?.subtitlePaths).toEqual([isRename ? output() : source()]);
      expect(state.attempts).toEqual([{ path: output(), overwrite: false }]);
    });

    it("ignores a pending output-directory pick after starting with the accepted directory", async () => {
      await prepare();
      const initialDirectory = inputPath("accepted-output");
      render().find(
        (entry) =>
          entry.type === "input" && entry.props.value === (isFont ? "chosen_dir" : "copy_to_chosen")
      )!.props.onChange!();
      state.open.mockResolvedValueOnce(initialDirectory);
      await labeled("btn_pick_chosen_dir").onClick!();
      let finishPick!: (path: string) => void;
      state.open.mockImplementation(
        () =>
          new Promise<string>((resolvePick) => {
            finishPick = resolvePick;
          })
      );
      const pendingPick = labeled("btn_pick_chosen_dir").onClick!();
      await labeled(button).onClick!();
      finishPick(inputPath("later-output"));
      await pendingPick;
      await labeled(button).onClick!();
      const expectedPath = `${initialDirectory}/${output().split("/").at(-1)!}`;
      expect(state.attempts).toEqual([
        { path: expectedPath, overwrite: false },
        { path: expectedPath, overwrite: true },
      ]);
      expect(state.files.get(source())).toBe(assSubtitle);
    });
  }
);

describe.each([
  {
    name: "Time Shift",
    component: TimingShift,
    button: "btn_save",
    batchButton: "btn_save_all",
    suffix: ".shifted.srt",
  },
  {
    name: "HDR Convert",
    component: HdrConvert,
    button: "btn_convert",
    batchButton: "btn_convert",
    suffix: ".hdr.ass",
  },
])("$name write preparation", ({ component, button, batchButton, suffix }) => {
  function render() {
    state.cursor = 0;
    return elements(component());
  }
  function labeled(label: string) {
    const match = render().find(
      (entry) => entry.type === "button" && entry.props.children === label
    );
    expect(match, label).toBeDefined();
    return match!.props;
  }
  const output = (stem = "episode") => inputPath(stem + suffix);

  it("writes a new output exclusively and preserves its input", async () => {
    await labeled(button).onClick!();
    expect(state.attempts).toEqual([{ path: output(), overwrite: false }]);
    expect(state.files.get(output())).not.toBe(subtitle);
    expect(state.files.get(state.paths[0]!)).toBe(subtitle);
    expect(state.ask).not.toHaveBeenCalled();
  });

  it("allows replacement of an ordinary output only after acceptance", async () => {
    state.files.set(output(), "previous output");
    await labeled(button).onClick!();
    expect(state.ask).toHaveBeenCalledOnce();
    expect(state.attempts).toEqual([{ path: output(), overwrite: true }]);
    expect(state.files.get(output())).not.toBe("previous output");
  });

  it.each([false, true])(
    "protects a source loaded in Batch Rename (directory alias=%s)",
    async (alias) => {
      const name = output().split("/").at(-1)!;
      const selected = alias ? inputPath(`rename-alias/${name}`) : output();
      if (alias) state.aliases.set(output(), selected);
      state.renameFiles = {
        subtitlePaths: [selected],
        subtitleNames: [name],
        videoPaths: [],
        videoNames: [],
      };
      state.files.set(output(), subtitle);
      await labeled(button).onClick!();
      expect(state.attempts).toEqual([]);
      expect(state.files.get(output())).toBe(subtitle);
      expect(state.files.get(state.paths[0]!)).toBe(subtitle);
      expect(state.ask).not.toHaveBeenCalled();
      state.renameFiles = null;
      await labeled(button).onClick!();
      expect(state.attempts).toEqual([{ path: output(), overwrite: true }]);
    }
  );

  it("preserves every selected source even with overwrite accepted", async () => {
    state.paths.push(output());
    state.files.set(output(), subtitle);
    await labeled(batchButton).onClick!();
    for (const path of state.paths) expect(state.files.get(path)).toBe(subtitle);
    expect(state.attempts.some((attempt) => state.paths.includes(attempt.path))).toBe(false);
    expect(state.ask).not.toHaveBeenCalled();
  });

  it("does not grant a late destination permission when another output was approved", async () => {
    const secondInput = inputPath("second.srt");
    state.paths.push(secondInput);
    state.files.set(secondInput, subtitle);
    state.files.set(output(), "approved old output");
    state.onRead = (path) => {
      if (path === secondInput) state.files.set(output("second"), "late file");
    };
    await labeled(batchButton).onClick!();
    expect(state.ask).toHaveBeenCalledOnce();
    expect(state.attempts).toEqual([
      { path: output(), overwrite: true },
      { path: output("second"), overwrite: false },
    ]);
    expect(state.files.get(output("second"))).toBe("late file");
  });

  it("declining overwrite preserves the destination and restores the idle controls", async () => {
    state.files.set(output(), "previous output");
    state.ask.mockResolvedValue(false);
    await labeled(button).onClick!();
    expect(state.attempts).toEqual([]);
    expect(state.files.get(output())).toBe("previous output");
    expect(labeled(button).disabled).toBe(false);
  });

  it("ignores a file picker that returns after the current batch begins", async () => {
    let finishPick!: (paths: string[]) => void;
    state.open.mockImplementation(
      () =>
        new Promise<string[]>((resolvePick) => {
          finishPick = resolvePick;
        })
    );
    const pendingPick = labeled("btn_select_files").onClick!();
    await labeled(button).onClick!();
    finishPick([inputPath("later.srt")]);
    await pendingPick;
    expect(state.setHdrFiles).not.toHaveBeenCalled();
    expect(state.setTimingFiles).not.toHaveBeenCalled();
    expect(state.attempts).toEqual([{ path: output(), overwrite: false }]);
  });

  it("disables Clear during delayed preparation and allows Cancel to stop the accepted operation", async () => {
    let finishProbe!: () => void;
    state.conflictProbe = () =>
      new Promise<string[]>((resolveProbe) => {
        finishProbe = () => resolveProbe([]);
      });
    const pending = labeled(button).onClick!();
    const clear = render().find((entry) => entry.props.title === "btn_clear_file");
    expect(clear?.props.disabled).toBe(true);
    labeled("btn_cancel").onClick!();
    finishProbe();
    await pending;
    expect(state.attempts).toEqual([]);
    expect(state.ask).not.toHaveBeenCalled();
    expect(labeled(button).disabled).toBe(false);
  });
});

it("ignores Timing's initial file read if saving starts before that read returns", async () => {
  const later = inputPath("later.srt");
  state.files.set(later, subtitle);
  state.open.mockResolvedValueOnce([later]);
  let finishRead!: () => void;
  let markReadStarted!: () => void;
  const readStarted = new Promise<void>((resolveRead) => {
    markReadStarted = resolveRead;
  });
  state.readProbe = (path) =>
    path === later
      ? new Promise<void>((resolveRead) => {
          finishRead = resolveRead;
          markReadStarted();
        })
      : Promise.resolve();
  function labeled(label: string) {
    state.cursor = 0;
    const match = elements(TimingShift()).find(
      (entry) => entry.type === "button" && entry.props.children === label
    );
    expect(match, label).toBeDefined();
    return match!.props;
  }
  const pendingPick = labeled("btn_select_files").onClick!();
  await readStarted;
  await labeled("btn_save").onClick!();
  finishRead();
  await pendingPick;
  expect(state.setTimingFiles).not.toHaveBeenCalled();
  expect(state.attempts).toEqual([{ path: inputPath("episode.shifted.srt"), overwrite: false }]);
});

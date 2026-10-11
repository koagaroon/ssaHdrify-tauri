import type { ReactElement } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import ThemeProvider from "./ThemeProvider";
import type { ThemeContextValue, ThemeMode, ThemePalette } from "./useTheme";

const hooks = vi.hoisted(() => ({
  values: [] as unknown[],
  stateCursor: 0,
  effectCursor: 0,
  effects: [] as {
    dependencies: readonly unknown[] | undefined;
    cleanup?: () => void;
  }[],
  layoutQueue: [] as (() => void)[],
  effectQueue: [] as (() => void)[],
}));

// Exercise the real provider and its effects in Node; browser rendering is checked separately.
vi.mock("react", async (importOriginal) => {
  const queueEffect = (
    effect: () => void | (() => void),
    dependencies: readonly unknown[] | undefined,
    queue: (() => void)[]
  ) => {
    const index = hooks.effectCursor++;
    const previous = hooks.effects[index];
    if (
      dependencies &&
      previous?.dependencies &&
      dependencies.length === previous.dependencies.length &&
      dependencies.every((value, i) => Object.is(value, previous.dependencies?.[i]))
    )
      return;
    queue.push(() => {
      previous?.cleanup?.();
      const slot: (typeof hooks.effects)[number] = { dependencies };
      const cleanup = effect();
      if (cleanup) slot.cleanup = cleanup;
      hooks.effects[index] = slot;
    });
  };
  return {
    ...(await importOriginal<typeof import("react")>()),
    useState: (initial: unknown) => {
      const index = hooks.stateCursor++;
      if (!(index in hooks.values))
        hooks.values[index] = typeof initial === "function" ? initial() : initial;
      return [
        hooks.values[index],
        (next: unknown) => {
          hooks.values[index] = typeof next === "function" ? next(hooks.values[index]) : next;
        },
      ];
    },
    useMemo: (compute: () => unknown) => compute(),
    useCallback: (callback: unknown) => callback,
    useEffect: (effect: () => void | (() => void), dependencies?: readonly unknown[]) =>
      queueEffect(effect, dependencies, hooks.effectQueue),
    useLayoutEffect: (effect: () => void | (() => void), dependencies?: readonly unknown[]) =>
      queueEffect(effect, dependencies, hooks.layoutQueue),
  };
});

const stored = new Map<string, string>();
const attributes = new Map<string, string>();
const listeners = new Set<(event: MediaQueryListEvent) => void>();
const storage = {
  getItem: vi.fn((key: string) => stored.get(key) ?? null),
  setItem: vi.fn((key: string, value: string) => {
    stored.set(key, value);
  }),
};
const media = {
  matches: false,
  addEventListener: vi.fn((_type: string, listener: (event: MediaQueryListEvent) => void) => {
    listeners.add(listener);
  }),
  removeEventListener: vi.fn((_type: string, listener: (event: MediaQueryListEvent) => void) => {
    listeners.delete(listener);
  }),
};

function renderProvider(): ThemeContextValue {
  hooks.stateCursor = 0;
  hooks.effectCursor = 0;
  const result = ThemeProvider({ children: null }) as ReactElement<{ value: ThemeContextValue }>;
  for (const effect of hooks.layoutQueue.splice(0)) effect();
  for (const effect of hooks.effectQueue.splice(0)) effect();
  return result.props.value;
}

function unmountProvider() {
  for (const effect of hooks.effects) effect?.cleanup?.();
  hooks.values = [];
  hooks.effects = [];
  hooks.layoutQueue = [];
  hooks.effectQueue = [];
}

function restartProvider() {
  unmountProvider();
  attributes.clear();
  return renderProvider();
}

function changeSystemAppearance(dark: boolean) {
  media.matches = dark;
  for (const listener of listeners) listener({ matches: dark } as MediaQueryListEvent);
  return renderProvider();
}

function expectAppearance(
  context: ThemeContextValue,
  mode: ThemeMode,
  palette: ThemePalette,
  appearance: "light" | "dark"
) {
  expect(context).toMatchObject({ mode, palette, appearance });
  expect(Object.fromEntries(attributes)).toEqual({
    "data-theme": appearance,
    "data-palette": palette,
  });
}

beforeEach(() => {
  stored.clear();
  attributes.clear();
  listeners.clear();
  vi.clearAllMocks();
  storage.getItem.mockImplementation((key: string) => stored.get(key) ?? null);
  storage.setItem.mockImplementation((key: string, value: string) => {
    stored.set(key, value);
  });
  media.matches = false;
  media.addEventListener.mockImplementation((_type, listener) => {
    listeners.add(listener);
  });
  vi.stubGlobal("localStorage", storage);
  vi.stubGlobal("window", { matchMedia: vi.fn(() => media) });
  vi.stubGlobal("document", {
    documentElement: {
      setAttribute: (name: string, value: string) => attributes.set(name, value),
    },
  });
});

afterEach(() => {
  unmountProvider();
  vi.unstubAllGlobals();
});

describe("appearance settings across an upgrade", () => {
  // Released v1.0.0 and v1.7.2 store these raw strings, without a palette key.
  for (const mode of ["light", "dark", "auto"] as const) {
    for (const systemDark of [false, true]) {
      it(`keeps legacy ${mode} with a ${systemDark ? "dark" : "light"} system and defaults to cool gray`, () => {
        stored.set("ssahdrify-theme", mode);
        media.matches = systemDark;

        const context = renderProvider();

        const appearance = mode === "auto" ? (systemDark ? "dark" : "light") : mode;
        expectAppearance(context, mode, "cool", appearance);
        expect(Object.fromEntries(stored)).toEqual({ "ssahdrify-theme": mode });
        expect(storage.setItem).not.toHaveBeenCalled();
      });
    }
  }

  it.each(["light", "dark", "auto"] as const)(
    "remembers a new warm palette without changing the legacy %s choice",
    (mode) => {
      stored.set("ssahdrify-theme", mode);
      media.matches = true;
      const appearance = mode === "auto" ? "dark" : mode;

      renderProvider().setPalette("warm");
      expectAppearance(renderProvider(), mode, "warm", appearance);
      expect(storage.setItem.mock.calls).toEqual([["ssahdrify-palette", "warm"]]);

      expectAppearance(restartProvider(), mode, "warm", appearance);
      expect(Object.fromEntries(stored)).toEqual({
        "ssahdrify-theme": mode,
        "ssahdrify-palette": "warm",
      });
    }
  );

  for (const palette of ["cool", "warm"] as const) {
    it(`keeps ${palette} when the mode changes and restores both after restart`, () => {
      stored.set("ssahdrify-theme", "dark");
      stored.set("ssahdrify-palette", palette);
      renderProvider().setMode("auto");

      expectAppearance(renderProvider(), "auto", palette, "light");
      expect(storage.setItem.mock.calls).toEqual([["ssahdrify-theme", "auto"]]);
      expectAppearance(changeSystemAppearance(true), "auto", palette, "dark");
      expectAppearance(restartProvider(), "auto", palette, "dark");
      expect(stored.get("ssahdrify-palette")).toBe(palette);
    });
  }

  it("remembers switching back from warm to cool without changing the automatic mode", () => {
    stored.set("ssahdrify-theme", "auto");
    stored.set("ssahdrify-palette", "warm");
    media.matches = true;
    expectAppearance(renderProvider(), "auto", "warm", "dark");

    renderProvider().setPalette("cool");
    expectAppearance(renderProvider(), "auto", "cool", "dark");
    expect(storage.setItem.mock.calls).toEqual([["ssahdrify-palette", "cool"]]);
    expectAppearance(restartProvider(), "auto", "cool", "dark");
  });

  it("follows system changes in auto while fixed modes retain their selected appearance", () => {
    stored.set("ssahdrify-theme", "auto");
    stored.set("ssahdrify-palette", "warm");
    expectAppearance(renderProvider(), "auto", "warm", "light");
    expectAppearance(changeSystemAppearance(true), "auto", "warm", "dark");
    expectAppearance(changeSystemAppearance(false), "auto", "warm", "light");

    renderProvider().setMode("dark");
    expectAppearance(changeSystemAppearance(true), "dark", "warm", "dark");
    expectAppearance(changeSystemAppearance(false), "dark", "warm", "dark");

    renderProvider().setMode("light");
    expectAppearance(changeSystemAppearance(true), "light", "warm", "light");
    renderProvider().setMode("auto");
    expectAppearance(renderProvider(), "auto", "warm", "dark");
  });
});

describe("unavailable or invalid appearance settings", () => {
  it.each([
    { saved: {}, mode: "light", palette: "cool" },
    { saved: { "ssahdrify-theme": "system" }, mode: "light", palette: "cool" },
    {
      saved: { "ssahdrify-theme": '"dark"', "ssahdrify-palette": "warm" },
      mode: "light",
      palette: "warm",
    },
    {
      saved: { "ssahdrify-theme": "dark", "ssahdrify-palette": "violet" },
      mode: "dark",
      palette: "cool",
    },
    {
      saved: { "ssahdrify-theme": "", "ssahdrify-palette": "" },
      mode: "light",
      palette: "cool",
    },
  ] as const)("uses independent defaults for $saved", ({ saved, mode, palette }) => {
    for (const [key, value] of Object.entries(saved)) stored.set(key, value);
    media.matches = true;

    expectAppearance(renderProvider(), mode, palette, mode);
    expect(storage.setItem).not.toHaveBeenCalled();
  });

  it.each(["ssahdrify-theme", "ssahdrify-palette", "both"])(
    "keeps the other preference when reading %s fails",
    (failedKey) => {
      stored.set("ssahdrify-theme", "dark");
      stored.set("ssahdrify-palette", "warm");
      storage.getItem.mockImplementation((key) => {
        if (failedKey === "both" || key === failedKey) throw new Error("Storage denied");
        return stored.get(key) ?? null;
      });
      const mode = failedKey === "ssahdrify-palette" ? "dark" : "light";
      const palette = failedKey === "ssahdrify-theme" ? "warm" : "cool";

      expectAppearance(renderProvider(), mode, palette, mode);
    }
  );

  it("continues applying both choices and system updates when storage writes fail", () => {
    stored.set("ssahdrify-theme", "light");
    storage.setItem.mockImplementation(() => {
      throw new Error("Storage is read-only");
    });
    const context = renderProvider();
    expect(() => context.setPalette("warm")).not.toThrow();
    expect(() => context.setMode("dark")).not.toThrow();
    expectAppearance(renderProvider(), "dark", "warm", "dark");

    renderProvider().setMode("auto");
    expectAppearance(changeSystemAppearance(true), "auto", "warm", "dark");
    expectAppearance(changeSystemAppearance(false), "auto", "warm", "light");
    expect(Object.fromEntries(stored)).toEqual({ "ssahdrify-theme": "light" });
  });

  it.each(["missing", "throwing"])(
    "falls back to light for auto when matchMedia is %s, while manual choices still work",
    (failure) => {
      stored.set("ssahdrify-theme", "auto");
      vi.stubGlobal(
        "window",
        failure === "missing"
          ? {}
          : {
              matchMedia: () => {
                throw new Error("Media queries denied");
              },
            }
      );

      expectAppearance(renderProvider(), "auto", "cool", "light");
      renderProvider().setMode("dark");
      renderProvider().setPalette("warm");
      expectAppearance(renderProvider(), "dark", "warm", "dark");
      expect(listeners.size).toBe(0);
    }
  );

  it("keeps the initial system appearance when subscribing to changes fails", () => {
    stored.set("ssahdrify-theme", "auto");
    media.matches = true;
    media.addEventListener.mockImplementation(() => {
      throw new Error("Subscription denied");
    });

    expectAppearance(renderProvider(), "auto", "cool", "dark");
    renderProvider().setMode("light");
    expectAppearance(renderProvider(), "light", "cool", "light");
  });
});

it("keeps one system listener across changes and removes it on unmount", () => {
  const context = renderProvider();
  context.setMode("auto");
  renderProvider();
  context.setPalette("warm");
  renderProvider();
  changeSystemAppearance(true);
  expect(listeners.size).toBe(1);
  expect(media.addEventListener).toHaveBeenCalledTimes(1);

  unmountProvider();
  expect(listeners.size).toBe(0);
  expect(media.removeEventListener).toHaveBeenCalledTimes(1);
  expect(media.removeEventListener.mock.calls[0]).toEqual(media.addEventListener.mock.calls[0]);

  expectAppearance(renderProvider(), "auto", "warm", "dark");
  expect(listeners.size).toBe(1);
  expect(media.addEventListener).toHaveBeenCalledTimes(2);
});

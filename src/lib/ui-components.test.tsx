import { createElement, type ErrorInfo, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import AppErrorBoundary from "../AppErrorBoundary";
import I18nProvider from "../i18n/I18nProvider";
import type { I18nContextValue } from "../i18n/useI18n";
import NitViz from "../features/hdr-convert/NitViz";
import NumberInput from "./NumberInput";
import { PreviewTable } from "./PreviewTable";
import StatusProvider from "./StatusProvider";
import type { StatusContextValue } from "./StatusContext";
import { useLogPanel } from "./useLogPanel";

const hooks = vi.hoisted(() => ({
  values: [] as unknown[],
  stateCursor: 0,
  refCursor: 0,
  refs: [] as { current: unknown }[],
  effectQueue: [] as (() => void | (() => void))[],
  cleanups: [] as (() => void)[],
}));

vi.mock("react", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react")>()),
  useState: (initial: unknown) => {
    const idx = hooks.stateCursor++;
    if (!(idx in hooks.values)) {
      hooks.values[idx] = typeof initial === "function" ? initial() : initial;
    }
    return [
      hooks.values[idx],
      (next: unknown) => {
        hooks.values[idx] = typeof next === "function" ? next(hooks.values[idx]) : next;
      },
    ];
  },
  useRef: (initial: unknown) => {
    const idx = hooks.refCursor++;
    if (!(idx in hooks.refs)) hooks.refs[idx] = { current: initial };
    return hooks.refs[idx];
  },
  useMemo: (compute: () => unknown) => compute(),
  useCallback: (cb: unknown) => cb,
  useContext: (ctx: { _currentValue: unknown }) => ctx._currentValue,
  useEffect: (effect: () => void | (() => void)) => {
    hooks.effectQueue.push(effect);
  },
}));

function resetHooks() {
  for (const cleanup of hooks.cleanups.splice(0)) cleanup();
  hooks.values = [];
  hooks.stateCursor = 0;
  hooks.refCursor = 0;
  hooks.refs = [];
  hooks.effectQueue = [];
}

function flushEffects() {
  for (const effect of hooks.effectQueue.splice(0)) {
    const cleanup = effect();
    if (typeof cleanup === "function") hooks.cleanups.push(cleanup);
  }
}

function walkElements(node: ReactNode): ReactElement<Record<string, unknown>>[] {
  if (Array.isArray(node)) return node.flatMap(walkElements);
  if (!node || typeof node !== "object" || !("props" in node)) return [];
  const el = node as ReactElement<Record<string, unknown>>;
  return [el, ...walkElements(el.props.children as ReactNode)];
}

beforeEach(() => {
  resetHooks();
});

afterEach(() => {
  resetHooks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("AppErrorBoundary", () => {
  it("renders children normally and switches to bilingual reload fallback on error", () => {
    const normal = new AppErrorBoundary({ children: createElement("div", null, "Healthy UI") });
    expect(renderToStaticMarkup(normal.render() as ReactElement)).toContain("Healthy UI");

    expect(AppErrorBoundary.getDerivedStateFromError()).toEqual({ hasError: true });
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    normal.componentDidCatch(new Error("boom"), { componentStack: "\n  at Broken" } as ErrorInfo);
    expect(spy).toHaveBeenCalledOnce();
    spy.mockRestore();

    normal.state = { hasError: true };
    const reload = vi.fn();
    vi.stubGlobal("window", { location: { reload } });
    const fallbackTree = normal.render() as ReactElement;
    const html = renderToStaticMarkup(fallbackTree);
    expect(html).toContain("SSA HDRify ran into a display error");
    expect(html).toContain("SSA HDRify 界面发生错误");

    const reloadBtn = walkElements(fallbackTree).find((el) => el.type === "button")!;
    (reloadBtn.props.onClick as () => void)();
    expect(reload).toHaveBeenCalledOnce();
  });
});

describe("StatusProvider", () => {
  it("skips no-op updates to preserve referential identity and updates changed tab statuses", () => {
    const renderStatus = () => {
      hooks.stateCursor = 0;
      hooks.refCursor = 0;
      const el = StatusProvider({ children: null }) as ReactElement<{ value: StatusContextValue }>;
      return el.props.value;
    };

    const initial = renderStatus();
    const initialMap = initial.statuses;
    expect(initialMap.hdr.kind).toBe("idle");

    // Identical update should keep the exact `statuses` object reference
    initial.setStatus("hdr", { kind: "idle", message: "" });
    expect(renderStatus().statuses).toBe(initialMap);

    // Progress update creates a new status entry for that tab only
    initial.setStatus("hdr", {
      kind: "busy",
      message: "Converting…",
      progress: { processed: 1, total: 3 },
    });
    const updated = renderStatus().statuses;
    expect(updated).not.toBe(initialMap);
    expect(updated.hdr).toEqual({
      kind: "busy",
      message: "Converting…",
      progress: { processed: 1, total: 3 },
    });
    // Repeat identical progress -> no-op
    initial.setStatus("hdr", {
      kind: "busy",
      message: "Converting…",
      progress: { processed: 1, total: 3 },
    });
    expect(renderStatus().statuses).toBe(updated);
  });
});

describe("I18nProvider", () => {
  it("defaults to zh when storage is empty or inaccessible, persists changes, and syncs <html lang>", () => {
    const stored = new Map<string, string>();
    const htmlAttrs = new Map<string, string>();
    vi.stubGlobal("localStorage", {
      getItem: (k: string) => stored.get(k) ?? null,
      setItem: (k: string, v: string) => stored.set(k, v),
    });
    vi.stubGlobal("document", {
      documentElement: {
        setAttribute: (k: string, v: string) => htmlAttrs.set(k, v),
      },
    });

    const renderI18n = () => {
      hooks.stateCursor = 0;
      hooks.refCursor = 0;
      const el = I18nProvider({ children: null }) as ReactElement<{ value: I18nContextValue }>;
      flushEffects();
      return el.props.value;
    };

    const ctx = renderI18n();
    expect(ctx.lang).toBe("zh");
    expect(htmlAttrs.get("lang")).toBe("zh");

    ctx.setLang("en");
    const updated = renderI18n();
    expect(updated.lang).toBe("en");
    expect(stored.get("ssahdrify-lang")).toBe("en");
    expect(htmlAttrs.get("lang")).toBe("en");
  });
});

describe("useLogPanel", () => {
  it("caps log length at 200 entries, truncates overlong entries without lone surrogates, and follows tail", () => {
    vi.useFakeTimers();
    function RenderLogs() {
      hooks.stateCursor = 0;
      hooks.refCursor = 0;
      const api = useLogPanel();
      flushEffects();
      return api;
    }

    const scrollBox = { scrollHeight: 500, scrollTop: 490, clientHeight: 0 };
    let api = RenderLogs();
    (api.logScrollRef as { current: unknown }).current = scrollBox;

    // Surrogate pair boundary at 4095: emoji "😀" takes 2 UTF-16 code units
    const astralFlood = "a".repeat(4094) + "😀".repeat(10);
    api.addLog(astralFlood, "warn");
    api = RenderLogs();
    expect(api.logs).toHaveLength(1);
    expect(api.logs[0]!.text.endsWith("…")).toBe(true);
    // Must not contain any lone UTF-16 surrogate
    expect(
      /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/.test(
        api.logs[0]!.text
      )
    ).toBe(false);

    vi.advanceTimersByTime(60);
    expect(scrollBox.scrollTop).toBe(500);

    // Push 205 more entries -> capped at 200
    for (let i = 0; i < 205; i++) {
      RenderLogs().addLog(`line-${i}`);
    }
    api = RenderLogs();
    expect(api.logs).toHaveLength(200);
    expect(api.logs[199]!.text).toBe("line-204");

    api.clearLogs();
    expect(RenderLogs().logs).toEqual([]);
  });
});

describe("NumberInput, PreviewTable, and NitViz", () => {
  it("preserves decimal precision and clamps min/max in NumberInput while hiding decorative spinners from ARIA", () => {
    const onChange = vi.fn();
    const tree = NumberInput({
      id: "size-input",
      value: "1.25",
      onChange,
      min: 0,
      max: 2,
      step: "0.5",
      invalid: true,
      ariaDescribedBy: "size-err",
    });
    const html = renderToStaticMarkup(tree);
    expect(html).toContain('aria-invalid="true"');
    expect(html).toContain('aria-describedby="size-err"');
    expect(html).toContain('aria-hidden="true"');

    const buttons = walkElements(tree).filter((el) => el.type === "button");
    expect(buttons).toHaveLength(2);
    // +0.5 on 1.25 preserves 2 decimal places -> "1.75"
    (buttons[0]!.props.onClick as () => void)();
    expect(onChange).toHaveBeenLastCalledWith("1.75");
    // -0.5 on 0.25 clamps to min 0 -> "0.00"
    const clamped = NumberInput({ value: "0.25", onChange, min: 0, max: 2, step: "0.5" });
    (walkElements(clamped).filter((el) => el.type === "button")[1]!.props.onClick as () => void)();
    expect(onChange).toHaveBeenLastCalledWith("0.00");
  });

  it("renders PreviewTable with semantic ARIA table roles for both populated and empty states", () => {
    const cols = [
      { key: "name", header: "File", render: (r: { name: string }) => r.name },
      { key: "status", header: "Status", render: () => "OK" },
    ];
    const emptyHtml = renderToStaticMarkup(
      PreviewTable({ rows: [], columns: cols, rowKey: (_, i) => String(i), emptyMessage: "Empty" })
    );
    expect(emptyHtml).toContain('role="table"');
    expect(emptyHtml).toContain('role="columnheader"');
    expect(emptyHtml).toContain("Empty");

    const filledHtml = renderToStaticMarkup(
      PreviewTable({
        rows: [{ name: "ep01.ass" }],
        columns: cols,
        rowKey: (r) => r.name,
        title: "Preview",
      })
    );
    expect(filledHtml).toContain('role="rowgroup"');
    expect(filledHtml).toContain('role="row"');
    expect(filledHtml).toContain('role="cell"');
    expect(filledHtml).toContain("ep01.ass");
  });

  it("drives NitViz keyboard slider navigation and preset buttons without stalling at low nits", () => {
    const onChange = vi.fn();
    hooks.stateCursor = 0;
    hooks.refCursor = 0;
    const tree = NitViz({ value: 1, onChange, disabled: false });
    const slider = walkElements(tree).find((el) => el.props.role === "slider")!;
    const preventDefault = vi.fn();

    // Near 1 nit, a small log step must still advance at least 1 nit instead of rounding back to 1
    (slider.props.onKeyDown as (e: unknown) => void)({
      key: "ArrowRight",
      shiftKey: false,
      preventDefault,
    });
    expect(preventDefault).toHaveBeenCalledOnce();
    expect(onChange).toHaveBeenLastCalledWith(2);

    (slider.props.onKeyDown as (e: unknown) => void)({
      key: "End",
      shiftKey: false,
      preventDefault,
    });
    expect(onChange).toHaveBeenLastCalledWith(10000);

    (slider.props.onKeyDown as (e: unknown) => void)({
      key: "Home",
      shiftKey: false,
      preventDefault,
    });
    expect(onChange).toHaveBeenLastCalledWith(1);

    // Preset click applies 203 nits (BT.2408)
    const preset203 = walkElements(tree).find(
      (el) => el.type === "button" && el.props["data-key"] === "bt2408"
    )!;
    (preset203.props.onClick as () => void)();
    expect(onChange).toHaveBeenLastCalledWith(203);
  });
});

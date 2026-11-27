import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { isRenderedFocusable, useModalFocusTrap } from "./useModalFocusTrap";

const hooks = vi.hoisted(() => ({
  effectCursor: 0,
  effects: [] as {
    dependencies: readonly unknown[] | undefined;
    cleanup?: () => void;
  }[],
  effectQueue: [] as (() => void)[],
  refs: [] as { current: unknown }[],
  refCursor: 0,
}));

vi.mock("react", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react")>()),
  useRef: (initial: unknown) => {
    const idx = hooks.refCursor++;
    if (!(idx in hooks.refs)) hooks.refs[idx] = { current: initial };
    return hooks.refs[idx];
  },
  useEffect: (effect: () => void | (() => void), dependencies?: readonly unknown[]) => {
    const idx = hooks.effectCursor++;
    const previous = hooks.effects[idx];
    if (
      dependencies &&
      previous?.dependencies &&
      dependencies.length === previous.dependencies.length &&
      dependencies.every((v, i) => Object.is(v, previous.dependencies?.[i]))
    ) {
      return;
    }
    hooks.effectQueue.push(() => {
      previous?.cleanup?.();
      const slot: (typeof hooks.effects)[number] = { dependencies };
      const cleanup = effect();
      if (cleanup) slot.cleanup = cleanup;
      hooks.effects[idx] = slot;
    });
  },
}));

interface FakeElement {
  id: string;
  focus: ReturnType<typeof vi.fn>;
  closest: (selector: string) => FakeElement | null;
  querySelector: (selector: string) => FakeElement | null;
  querySelectorAll: (selector: string) => FakeElement[];
  contains: (target: unknown) => boolean;
  getClientRects: () => { width: number }[];
}

function makeFakeElement(
  id: string,
  opts: {
    visible?: boolean;
    closedDetails?: FakeElement | null;
    summaryChild?: FakeElement | null;
    children?: FakeElement[];
  } = {}
): FakeElement {
  const children = opts.children ?? [];
  const el: FakeElement = {
    id,
    focus: vi.fn(() => {
      docState.activeElement = el;
    }),
    closest: (selector: string) =>
      selector === "details:not([open])" ? (opts.closedDetails ?? null) : null,
    querySelector: (selector: string) =>
      selector === ":scope > summary" ? (opts.summaryChild ?? null) : null,
    querySelectorAll: () => children,
    contains: (target: unknown) => target === el || children.includes(target as FakeElement),
    getClientRects: () => (opts.visible === false ? [] : [{ width: 100 }]),
  };
  return el;
}

const docState = {
  activeElement: null as FakeElement | null,
};
const keyListeners = new Set<(event: KeyboardEvent) => void>();

beforeEach(() => {
  hooks.effectCursor = 0;
  hooks.effects = [];
  hooks.effectQueue = [];
  hooks.refs = [];
  hooks.refCursor = 0;
  docState.activeElement = null;
  keyListeners.clear();

  vi.stubGlobal("document", docState);
  vi.stubGlobal("window", {
    addEventListener: vi.fn((type: string, fn: (event: KeyboardEvent) => void) => {
      if (type === "keydown") keyListeners.add(fn);
    }),
    removeEventListener: vi.fn((type: string, fn: (event: KeyboardEvent) => void) => {
      if (type === "keydown") keyListeners.delete(fn);
    }),
  });
});

afterEach(() => {
  for (const effect of hooks.effects) effect?.cleanup?.();
  vi.unstubAllGlobals();
});

function dispatchKey(key: string, shiftKey = false) {
  const event = {
    key,
    shiftKey,
    preventDefault: vi.fn(),
    stopPropagation: vi.fn(),
  } as unknown as KeyboardEvent;
  for (const listener of keyListeners) listener(event);
  return event;
}

describe("useModalFocusTrap", () => {
  it("focuses initialFocusRef on open, traps Tab/Shift+Tab, handles Escape, and restores focus on close", () => {
    const trigger = makeFakeElement("trigger");
    const closeBtn = makeFakeElement("close");
    const hiddenBtn = makeFakeElement("hidden", { visible: false });
    const actionBtn = makeFakeElement("action");
    const dialog = makeFakeElement("dialog", { children: [closeBtn, hiddenBtn, actionBtn] });
    docState.activeElement = trigger;

    const dialogRef = { current: dialog as unknown as HTMLElement };
    const initialFocusRef = { current: closeBtn as unknown as HTMLElement };
    const onEscape1 = vi.fn();
    const onEscape2 = vi.fn();

    function RenderTrap(open: boolean, onEscape: () => void) {
      hooks.refCursor = 0;
      hooks.effectCursor = 0;
      useModalFocusTrap({ open, dialogRef, initialFocusRef, onEscape });
      for (const fn of hooks.effectQueue.splice(0)) fn();
    }

    RenderTrap(true, onEscape1);
    expect(closeBtn.focus).toHaveBeenCalledTimes(1);
    expect(docState.activeElement).toBe(closeBtn);

    // Updating onEscape identity must NOT re-focus closeBtn or restore focus early
    docState.activeElement = actionBtn;
    RenderTrap(true, onEscape2);
    expect(closeBtn.focus).toHaveBeenCalledTimes(1);
    expect(trigger.focus).not.toHaveBeenCalled();

    // Forward Tab from last focusable wraps to first
    const tabForward = dispatchKey("Tab", false);
    expect(tabForward.preventDefault).toHaveBeenCalledOnce();
    expect(docState.activeElement).toBe(closeBtn);

    // Backward Shift+Tab from first wraps to last (skipping invisible hiddenBtn)
    const tabBackward = dispatchKey("Tab", true);
    expect(tabBackward.preventDefault).toHaveBeenCalledOnce();
    expect(docState.activeElement).toBe(actionBtn);

    // Escape calls latest onEscape2 and stops propagation
    const esc = dispatchKey("Escape");
    expect(esc.preventDefault).toHaveBeenCalledOnce();
    expect(esc.stopPropagation).toHaveBeenCalledOnce();
    expect(onEscape1).not.toHaveBeenCalled();
    expect(onEscape2).toHaveBeenCalledOnce();

    // Closing restores focus to trigger
    RenderTrap(false, onEscape2);
    expect(trigger.focus).toHaveBeenCalledOnce();
    expect(keyListeners.size).toBe(0);
  });

  it("excludes children inside closed <details> while keeping the <summary> focusable", () => {
    const summary = makeFakeElement("summary");
    const closedDetails = makeFakeElement("details", { summaryChild: summary });
    summary.closest = () => closedDetails;
    const innerButton = makeFakeElement("inner-btn", { closedDetails });

    expect(isRenderedFocusable(summary as unknown as HTMLElement)).toBe(true);
    expect(isRenderedFocusable(innerButton as unknown as HTMLElement)).toBe(false);
  });
});

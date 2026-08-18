import { describe, expect, it } from "vitest";

import { completeFontCacheProbe, isFontCacheUnavailable } from "./font-cache-ui-state";

describe("font cache UI state", () => {
  it("does not let StrictMode's cancelled first setup suppress the live retry", () => {
    const checked = { current: false };

    completeFontCacheProbe(checked, true);
    expect(checked.current).toBe(false);

    completeFontCacheProbe(checked, false);
    expect(checked.current).toBe(true);
  });

  it("distinguishes unavailable cache state from schema recovery", () => {
    expect(isFontCacheUnavailable(null, false)).toBe(false);
    expect(isFontCacheUnavailable(null, true)).toBe(true);
    expect(
      isFontCacheUnavailable(
        { available: false, schemaMismatch: false, path: "cache.sqlite" },
        false
      )
    ).toBe(true);
    expect(
      isFontCacheUnavailable(
        { available: false, schemaMismatch: true, path: "cache.sqlite" },
        false
      )
    ).toBe(false);
    expect(
      isFontCacheUnavailable(
        { available: true, schemaMismatch: false, path: "cache.sqlite" },
        false
      )
    ).toBe(false);
  });
});

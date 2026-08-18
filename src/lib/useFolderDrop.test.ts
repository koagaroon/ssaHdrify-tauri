import { describe, expect, it } from "vitest";

import { isDropPositionInsideRect } from "./useFolderDrop";

const visibleRect = {
  left: 10,
  right: 110,
  top: 20,
  bottom: 70,
  width: 100,
  height: 50,
};

describe("drop-zone hit testing", () => {
  it("converts physical cursor coordinates to CSS pixels", () => {
    expect(isDropPositionInsideRect({ x: 40, y: 60 }, visibleRect, 2)).toBe(true);
    expect(isDropPositionInsideRect({ x: 10, y: 60 }, visibleRect, 2)).toBe(false);
  });

  it("rejects zero-area hidden tab drop zones, including at the origin", () => {
    const hiddenRect = { left: 0, right: 0, top: 0, bottom: 0, width: 0, height: 0 };
    expect(isDropPositionInsideRect({ x: 0, y: 0 }, hiddenRect, 1)).toBe(false);
  });

  it("keeps inclusive edge hits for a visible drop zone", () => {
    expect(isDropPositionInsideRect({ x: 10, y: 20 }, visibleRect, 1)).toBe(true);
    expect(isDropPositionInsideRect({ x: 110, y: 70 }, visibleRect, 1)).toBe(true);
  });
});

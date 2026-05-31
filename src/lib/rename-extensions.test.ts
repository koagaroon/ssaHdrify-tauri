import { describe, expect, it } from "vitest";

import { categorize, categorizeForRename } from "./rename-extensions";

describe("rename extension classification", () => {
  it("keeps .sup out of parser-aligned subtitle workflows", () => {
    expect(categorize("episode.sup")).toBe("unknown");
  });

  it("allows .sup only for Batch Rename sidecar classification", () => {
    expect(categorizeForRename("episode.sup")).toBe("subtitle");
    expect(categorizeForRename("episode.SUP")).toBe("subtitle");
  });
});

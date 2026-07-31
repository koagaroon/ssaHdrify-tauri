import { describe, expect, it } from "vitest";
import { resolveAppModalVisibility } from "./modal-coordination";

describe("app modal coordination", () => {
  it("shows neither dialog when neither is requested", () => {
    expect(resolveAppModalVisibility(false, false)).toEqual({ cache: false, licenses: false });
  });

  it("defers an asynchronous cache prompt until Licenses closes", () => {
    expect(resolveAppModalVisibility(false, true)).toEqual({ cache: false, licenses: true });
    expect(resolveAppModalVisibility(true, true)).toEqual({ cache: false, licenses: true });
    expect(resolveAppModalVisibility(true, false)).toEqual({ cache: true, licenses: false });
  });

  it("gives an already-pending cache prompt exclusive ownership", () => {
    expect(resolveAppModalVisibility(true, false)).toEqual({ cache: true, licenses: false });
  });
});

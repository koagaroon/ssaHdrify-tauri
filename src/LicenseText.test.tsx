import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import LicenseText from "./LicenseText";
import { LICENSE_NOTICES } from "./lib/license-notices";
import { LICENSE_PAGE_SIZE } from "./lib/license-pages";

describe("license reader rendering", () => {
  it("renders only the selected component, keeping the complete inventory out of the DOM", () => {
    const notice = LICENSE_NOTICES.find((item) => item.id === "frontend")!;
    expect(notice.text.length).toBeGreaterThan(1_000_000);
    const markup = renderToStaticMarkup(<LicenseText text={notice.text} components />);
    expect(markup).toContain("<select");
    expect(markup).toContain("Rust standard library");
    expect(markup.length).toBeLessThan(100_000);
  });

  it("provides page controls for long notices while displaying a bounded first page", () => {
    const markup = renderToStaticMarkup(<LicenseText text={"x".repeat(LICENSE_PAGE_SIZE + 1)} />);
    expect(markup).toContain("Page 1 of 2");
    expect(markup).toContain("Next page");
    expect(markup).not.toContain("x".repeat(LICENSE_PAGE_SIZE + 1));
  });

  it("renders upstream markup as inert text", () => {
    const markup = renderToStaticMarkup(<LicenseText text={'<script>alert("test")</script>'} />);
    expect(markup).toContain("&lt;script&gt;");
    expect(markup).not.toContain("<script>");
  });
});

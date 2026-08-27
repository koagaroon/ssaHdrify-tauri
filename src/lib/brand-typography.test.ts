/// <reference types="node" />

import { readFile } from "node:fs/promises";
import { describe, expect, it } from "vitest";
import { strings } from "../i18n/strings";

async function readCss(relativePath: string): Promise<string> {
  return readFile(new URL(relativePath, import.meta.url), "utf8");
}

function ruleBody(css: string, selector: string): string {
  const escapedSelector = selector.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&");
  const match = new RegExp(`(?:^|\\n)${escapedSelector}\\s*\\{([^}]*)\\}`, "u").exec(css);
  expect(match, `Expected a ${selector} CSS rule`).not.toBeNull();
  return match?.[1] ?? "";
}

describe("brand typography contract", () => {
  it("keeps the product title identical across locales", () => {
    const appTitle = strings["app_title"]!;

    expect(appTitle.en).toBe("SSA HDRify");
    expect(appTitle.zh).toBe(appTitle.en);
  });

  it("keeps the Latin wordmark stable while localizing genuine Chinese copy", async () => {
    const [tokens, shell] = await Promise.all([readCss("../index.css"), readCss("../shell.css")]);

    expect(tokens).toMatch(/--font-brand:\s*"Inter"/u);
    expect(ruleBody(tokens, ":lang(zh)")).toContain('--font-display: "Smiley Sans"');
    expect(ruleBody(tokens, ":lang(zh)")).not.toContain("--font-brand");

    expect(ruleBody(shell, ".app-title")).toContain("font-family: var(--font-brand)");
    expect(shell).not.toMatch(/:lang\(zh\)\s+\.app-title/u);
    expect(ruleBody(shell, ".app-tagline")).toContain("font-family: var(--font-display)");
    expect(ruleBody(shell, ":lang(zh) .app-tagline")).toContain("font-style: oblique");
  });
});

import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, it, vi } from "vitest";
import { resolveAppVersion } from "./app-version.mjs";

describe("resolveAppVersion", () => {
  it("resolves a sanitized version label from the active repository", () => {
    const repoRoot = fileURLToPath(new URL("../../", import.meta.url));
    const resolved = resolveAppVersion(repoRoot);
    assert.match(resolved, /^[a-zA-Z0-9._+-]+$/u);
    assert.notEqual(resolved, "v0.0.0-unknown");
  });

  it("falls back to v<package.json version> when git is unavailable in a directory", () => {
    const dir = mkdtempSync(join(tmpdir(), "app-version-pkg-"));
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      writeFileSync(join(dir, "package.json"), JSON.stringify({ version: "2.4.6-rc.1" }), "utf8");
      assert.equal(resolveAppVersion(dir), "v2.4.6-rc.1");
    } finally {
      warnSpy.mockRestore();
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("falls back to v0.0.0-unknown when both git and package.json version are missing or empty", () => {
    const dir = mkdtempSync(join(tmpdir(), "app-version-empty-"));
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      assert.equal(resolveAppVersion(dir), "v0.0.0-unknown");
      writeFileSync(join(dir, "package.json"), JSON.stringify({ version: "" }), "utf8");
      assert.equal(resolveAppVersion(dir), "v0.0.0-unknown");
    } finally {
      warnSpy.mockRestore();
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("strips unsafe HTML/shell characters from package.json version strings", () => {
    const dir = mkdtempSync(join(tmpdir(), "app-version-sanitize-"));
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      writeFileSync(
        join(dir, "package.json"),
        JSON.stringify({ version: '1.9.0"<script>alert(1)</script>' }),
        "utf8"
      );
      assert.equal(resolveAppVersion(dir), "v1.9.0scriptalert1script");
    } finally {
      warnSpy.mockRestore();
      rmSync(dir, { recursive: true, force: true });
    }
  });
});

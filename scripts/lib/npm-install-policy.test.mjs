import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, test } from "vitest";
import { assertNpmInstallManifest, npmInstallLifecycleScripts } from "../check-npm-install.mjs";

/** @param {unknown} manifest @param {boolean} hasBindingGyp */
function runFreshCheckout(manifest, hasBindingGyp = false) {
  const root = mkdtempSync(join(tmpdir(), "ssahdrify-install-policy-"));
  try {
    mkdirSync(join(root, "scripts"));
    const script = join(root, "scripts", "check-npm-install.mjs");
    copyFileSync(new URL("../check-npm-install.mjs", import.meta.url), script);
    writeFileSync(join(root, "package.json"), JSON.stringify(manifest), "utf8");
    if (hasBindingGyp) writeFileSync(join(root, "binding.gyp"), "{}", "utf8");
    return spawnSync(process.execPath, [script], {
      cwd: root,
      encoding: "utf8",
      timeout: 10_000,
      windowsHide: true,
    });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

describe("the dependency-free npm installation preflight", () => {
  test("runs directly before every CI dependency installation", () => {
    const workflow = readFileSync(
      new URL("../../.github/workflows/ci.yml", import.meta.url),
      "utf8"
    );
    const installs = [...workflow.matchAll(/^ {8}run: npm ci\r?$/gmu)];
    expect(installs.length).toBeGreaterThan(0);
    for (const install of installs) {
      expect(workflow.slice(0, install.index)).toMatch(
        /run: node scripts\/check-npm-install\.mjs\r?\n\r?\n {6}- name: [^\r\n]+\r?\n$/u
      );
    }
  });

  test("runs before node_modules exists in a fresh checkout", () => {
    const result = runFreshCheckout({ name: "installation-fixture", scripts: { build: "vite" } });
    expect(result.error).toBeUndefined();
    expect(result.status).toBe(0);
    expect(result.stdout).toContain("installation boundary passed");
  });

  test.each(npmInstallLifecycleScripts)("rejects the root %s lifecycle hook", (hook) => {
    expect(() =>
      assertNpmInstallManifest({ scripts: { [hook]: "node install-helper.mjs" } })
    ).toThrow("installation scripts require review");
  });

  test("fails the fresh-checkout command when an install hook is present", () => {
    const result = runFreshCheckout({ scripts: { postinstall: "node install-helper.mjs" } });
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("postinstall");
  });

  test("rejects binding.gyp before npm can synthesize an install script", () => {
    const result = runFreshCheckout({ scripts: {} }, true);
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("implicit install");
  });

  test("requires review before introducing workspace install scripts", () => {
    expect(() => assertNpmInstallManifest({ workspaces: ["packages/*"] })).toThrow(
      "workspaces require"
    );
  });

  test.each([null, [], { scripts: null }, { scripts: [] }])(
    "rejects malformed manifests: %j",
    (manifest) => {
      expect(() => assertNpmInstallManifest(manifest)).toThrow("valid");
    }
  );
});

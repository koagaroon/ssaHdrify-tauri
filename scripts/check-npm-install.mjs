import { lstatSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const npmInstallLifecycleScripts = Object.freeze([
  "preinstall",
  "install",
  "postinstall",
  "prepublish",
  "preprepare",
  "prepare",
  "postprepare",
  "predependencies",
  "dependencies",
  "postdependencies",
]);

/** @param {unknown} manifest @param {boolean} hasBindingGyp */
export function assertNpmInstallManifest(manifest, hasBindingGyp = false) {
  if (manifest === null || typeof manifest !== "object" || Array.isArray(manifest)) {
    throw new Error("Installation preflight requires a valid package manifest");
  }
  if (Object.hasOwn(manifest, "workspaces")) {
    throw new Error("npm workspaces require an installation-policy review");
  }
  const scripts = "scripts" in manifest ? manifest.scripts : {};
  if (scripts === null || typeof scripts !== "object" || Array.isArray(scripts)) {
    throw new Error("Installation preflight requires a valid scripts object");
  }
  const hooks = npmInstallLifecycleScripts.filter((name) => Object.hasOwn(scripts, name));
  if (hooks.length > 0) {
    throw new Error(`Root npm installation scripts require review: ${hooks.join(", ")}`);
  }
  if (hasBindingGyp) {
    throw new Error("Root binding.gyp requires review before npm can run an implicit install");
  }
}

/** @param {string} root */
export function checkNpmInstallBoundary(root) {
  let hasBindingGyp = true;
  try {
    lstatSync(join(root, "binding.gyp"));
  } catch (error) {
    if (error && typeof error === "object" && "code" in error && error.code === "ENOENT") {
      hasBindingGyp = false;
    } else {
      throw error;
    }
  }
  assertNpmInstallManifest(
    JSON.parse(readFileSync(join(root, "package.json"), "utf8")),
    hasBindingGyp
  );
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    checkNpmInstallBoundary(fileURLToPath(new URL("../", import.meta.url)));
    console.log("Project installation boundary passed.");
  } catch (error) {
    console.error(error instanceof Error ? error.message : "Installation preflight failed");
    process.exitCode = 1;
  }
}

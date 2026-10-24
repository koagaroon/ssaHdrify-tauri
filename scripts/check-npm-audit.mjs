import { execFile } from "node:child_process";
import { lstat, readFile, realpath } from "node:fs/promises";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { checkNpmInstallBoundary } from "./check-npm-install.mjs";
import { evaluateNpmAudit, isObject } from "./lib/npm-audit-policy.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
const maxBytes = 8 * 1024 * 1024;

/** @param {string} path */
async function readText(path) {
  const stat = await lstat(path);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size > maxBytes) {
    throw new Error("Audit input is not a bounded regular file");
  }
  return readFile(path, "utf8");
}

/** @param {string} npmCli @returns {Promise<{ stdout: string, exitCode: number }>} */
function runAudit(npmCli) {
  return new Promise((resolveAudit, reject) => {
    execFile(
      process.execPath,
      [
        npmCli,
        "audit",
        "--json",
        "--include=dev",
        "--include=optional",
        "--include=peer",
        "--audit-level=info",
      ],
      { cwd: root, encoding: "utf8", timeout: 120_000, maxBuffer: maxBytes, windowsHide: true },
      (error, stdout) => {
        const exitCode = error ? error.code : 0;
        if (error?.killed || typeof exitCode !== "number" || !Number.isInteger(exitCode)) {
          reject(new Error("npm audit could not complete within the process/output limits"));
        } else {
          resolveAudit({ stdout, exitCode });
        }
      }
    );
  });
}

async function main() {
  checkNpmInstallBoundary(root);
  /** @type {unknown} */
  const manifest = JSON.parse(await readText(join(root, "package.json")));
  if (!isObject(manifest)) throw new Error("Invalid package manifest");
  const npmCli = process.env.npm_execpath;
  if (!npmCli || !isAbsolute(npmCli))
    throw new Error("Run this check with npm run audit:dependencies");
  const npmRoot = resolve(dirname(npmCli), "..");
  /** @type {unknown} */
  const npmManifest = JSON.parse(await readText(join(npmRoot, "package.json")));
  if (
    !isObject(npmManifest) ||
    npmManifest.name !== "npm" ||
    typeof npmManifest.version !== "string" ||
    manifest.packageManager !== `npm@${npmManifest.version}` ||
    !isObject(npmManifest.bin) ||
    typeof npmManifest.bin.npm !== "string" ||
    (await realpath(resolve(npmRoot, npmManifest.bin.npm))) !== (await realpath(npmCli))
  ) {
    throw new Error("Run the audit with the exact npm version declared in packageManager");
  }
  evaluateNpmAudit(await runAudit(npmCli));
  console.log("npm audit: no vulnerabilities reported in the complete dependency graph.");
}

try {
  await main();
} catch (error) {
  console.error(error instanceof Error ? error.message : "npm audit check failed");
  process.exitCode = 1;
}

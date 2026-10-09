import { execFile } from "node:child_process";
import { lstat, readFile, readdir, realpath } from "node:fs/promises";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { evaluateNpmAudit, isObject, readAuditException } from "./lib/npm-audit-policy.mjs";

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

/** @param {string} directory @param {Record<string, string>} sources */
async function inspectSources(directory, sources = {}) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isSymbolicLink()) throw new Error("Source links require audit boundary review");
    if (entry.isDirectory()) await inspectSources(path, sources);
    else if (/\.[cm]?[jt]sx?$/u.test(entry.name)) {
      if (Object.keys(sources).length >= 2000) throw new Error("Source inspection limit exceeded");
      sources[path.slice(root.length).replace(/\\/gu, "/")] = await readText(path);
    }
  }
  return sources;
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
  const exception = readAuditException(
    JSON.parse(await readText(join(root, "scripts/npm-audit-exceptions.json")))
  );
  /** @type {Record<string, string>} */
  const boundaryFiles = {};
  for (const path of Object.keys(exception.boundaryFiles ?? {})) {
    if (!/^[\w./-]+$/u.test(path) || path.split("/").some((part) => part === ".." || part === "")) {
      throw new Error("Invalid audit boundary path");
    }
    boundaryFiles[path] = await readText(join(root, path));
  }
  const configFiles = (await readdir(root)).filter((name) =>
    /^(?:\.stylelint.*|stylelint\.config\..*|tsconfig.*\.json)$/u.test(name)
  );
  if (configFiles.some((path) => !Object.hasOwn(boundaryFiles, path))) {
    throw new Error("Additional lint or TypeScript configuration requires audit boundary review");
  }
  const sources = await inspectSources(join(root, "src"));
  await inspectSources(join(root, "public"), sources);
  /** @type {unknown} */
  const lockfile = JSON.parse(await readText(join(root, "package-lock.json")));
  const result = evaluateNpmAudit({
    ...(await runAudit(npmCli)),
    manifest,
    lockfile,
    exception,
    boundaryFiles,
    sources,
  });
  if (result.warning) console.warn(result.warning);
  else console.log("npm audit: no vulnerabilities reported in the complete dependency graph.");
}

try {
  await main();
} catch (error) {
  console.error(error instanceof Error ? error.message : "npm audit check failed");
  process.exitCode = 1;
}

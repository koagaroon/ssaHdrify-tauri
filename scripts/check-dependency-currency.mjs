import { execFile } from "node:child_process";
import { appendFile, readFile, readdir } from "node:fs/promises";
import { relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

import {
  MonitorDataError,
  classifyActionCurrency,
  classifyCargoCurrency,
  classifyNpmCurrency,
  collectCargoTargets,
  collectNpmTargets,
  cratesIndexPath,
  dependencyWatchExitCode,
  fetchTextWithPolicy,
  inspectGitHubReleases,
  inspectNpmMetadata,
  inspectResolvedCommitSha,
  isRecord,
  mapWithConcurrency,
  parseActionReferences,
  parseCratesIndex,
  parseStableVersion,
  renderDependencyReport,
} from "./lib/dependency-currency.mjs";

const projectRoot = fileURLToPath(new URL("../", import.meta.url));
const packageJsonPath = resolve(projectRoot, "package.json");
const packageLockPath = resolve(projectRoot, "package-lock.json");
const cargoManifestPath = resolve(projectRoot, "src-tauri", "Cargo.toml");
const workflowRoots = [
  resolve(projectRoot, ".github", "workflows"),
  resolve(projectRoot, ".github", "actions"),
];

const userAgent = "ssaHdrify-dependency-watch/1.0 (https://github.com/koagaroon/ssaHdrify-tauri)";
const npmHeaders = {
  Accept: "application/vnd.npm.install-v1+json",
  "User-Agent": userAgent,
};
const cratesHeaders = {
  Accept: "text/plain",
  "User-Agent": userAgent,
};
/** @type {Record<string, string>} */
const githubHeaders = {
  Accept: "application/vnd.github+json",
  "X-GitHub-Api-Version": "2026-03-10",
  "User-Agent": userAgent,
};
if (typeof process.env.GITHUB_TOKEN === "string" && process.env.GITHUB_TOKEN.trim() !== "") {
  githubHeaders.Authorization = `Bearer ${process.env.GITHUB_TOKEN.trim()}`;
}

const supportedAliasNames = new Set(["@typescript/native", "typescript"]);
const typescriptTargets = [
  {
    dependency: "TypeScript 7 release compiler",
    registryPackage: "typescript",
    lockPath: "node_modules/@typescript/native",
    expectedPackageName: "typescript",
    line: null,
  },
  {
    dependency: "TypeScript 6 compatibility wrapper",
    registryPackage: "@typescript/typescript6",
    lockPath: "node_modules/typescript",
    expectedPackageName: "@typescript/typescript6",
    line: null,
  },
  {
    dependency: "TypeScript 6 effective compiler/API",
    registryPackage: "typescript",
    lockPath: "node_modules/@typescript/old",
    expectedPackageName: "typescript",
    line: { major: 6 },
  },
];

/** @type {Record<string, {mode: "manual" | "hold-line", reason: string, reviewedThrough?: string}>} */
const cargoPolicies = {
  deno_core: {
    mode: /** @type {const} */ ("manual"),
    reason:
      "Every deno_core release requires a manual V8, API, and runtime audit before updating the exact pin.",
    reviewedThrough: "0.410.0",
  },
  rusqlite: {
    mode: /** @type {const} */ ("hold-line"),
    reason:
      "Keep 0.39 until rusqlite bundles SQLite 3.53.4 or newer; 0.40.2 passed the Rust 1.91 gate but bundles SQLite 3.53.2.",
  },
  rfd: {
    mode: /** @type {const} */ ("hold-line"),
    reason:
      "Keep 0.16 until tauri-plugin-dialog officially supports rfd 0.17; direct defaults stay disabled so the plugin alone selects the shared Linux backend.",
  },
};

/** @typedef {import("./lib/dependency-currency.mjs").CurrencyRow} CurrencyRow */

/** @param {string} path */
async function readJson(path) {
  let text;
  try {
    text = await readFile(path, "utf8");
  } catch {
    throw new MonitorDataError(`Unable to read ${relative(projectRoot, path)}`);
  }
  try {
    return JSON.parse(text);
  } catch {
    throw new MonitorDataError(`${relative(projectRoot, path)} contains invalid JSON`);
  }
}

/** @param {unknown} error */
function safeErrorReason(error) {
  return error instanceof MonitorDataError
    ? error.message
    : "The local monitor encountered an unexpected failure.";
}

/**
 * @param {string} ecosystem
 * @param {string} dependency
 * @param {string} locked
 * @param {unknown} error
 * @returns {CurrencyRow}
 */
function errorRow(ecosystem, dependency, locked, error) {
  return {
    ecosystem,
    dependency,
    locked,
    latest: "unknown",
    status: "error",
    reason: safeErrorReason(error),
  };
}

const responseCache = new Map();

/**
 * @param {string} url
 * @param {Record<string, string>} headers
 * @param {number} maxBytes
 */
function cachedFetchText(url, headers, maxBytes) {
  const key = `${headers.Accept ?? ""}\n${url}`;
  const existing = responseCache.get(key);
  if (existing !== undefined) return existing;
  const request = fetchTextWithPolicy(url, { headers, maxBytes });
  responseCache.set(key, request);
  return request;
}

/**
 * @param {string} packageName
 * @returns {Promise<unknown>}
 */
async function fetchNpmMetadata(packageName) {
  const encodedName = encodeURIComponent(packageName);
  const text = await cachedFetchText(
    `https://registry.npmjs.org/${encodedName}`,
    npmHeaders,
    32 * 1024 * 1024
  );
  try {
    return JSON.parse(text);
  } catch {
    throw new MonitorDataError(`npm returned malformed JSON for ${packageName}`);
  }
}

/** @returns {Promise<CurrencyRow[]>} */
async function checkNpmDependencies() {
  const [packageJson, lockfile] = await Promise.all([
    readJson(packageJsonPath),
    readJson(packageLockPath),
  ]);
  const { targets, aliases } = collectNpmTargets(packageJson, lockfile);
  if (!isRecord(packageJson) || !isRecord(packageJson.allowScripts)) {
    throw new MonitorDataError("package.json must contain the reviewed allowScripts map");
  }
  const scriptApprovalNames = new Set(
    Object.keys(packageJson.allowScripts).map((approval) => {
      const versionSeparator = approval.lastIndexOf("@");
      return versionSeparator > 0 ? approval.slice(0, versionSeparator) : approval;
    })
  );
  /** @type {CurrencyRow[]} */
  const rows = [];

  for (const alias of aliases) {
    if (!supportedAliasNames.has(alias)) {
      rows.push(
        errorRow(
          "npm / toolchain",
          alias,
          "alias",
          new MonitorDataError("This npm alias has no dedicated currency rule.")
        )
      );
    }
  }
  for (const supportedAlias of supportedAliasNames) {
    if (!aliases.includes(supportedAlias)) {
      rows.push(
        errorRow(
          "npm / toolchain",
          supportedAlias,
          "missing",
          new MonitorDataError("Expected TypeScript alias is missing from package.json.")
        )
      );
    }
  }

  const genericRows = await mapWithConcurrency(targets, 6, async (target) => {
    try {
      const metadata = await fetchNpmMetadata(target.name);
      const isNodeTypes = target.name === "@types/node";
      const inspection = inspectNpmMetadata(
        metadata,
        target.name,
        isNodeTypes ? { major: 22, minor: 13 } : null
      );
      const result = classifyNpmCurrency({
        locked: target.locked,
        inspection,
        linePolicy: isNodeTypes
          ? {
              outsideLine: "held",
              reason:
                "Later @types/node lines are held so type-checking continues to enforce the Node 22.13 API floor.",
            }
          : null,
      });
      if (result.status === "update" && scriptApprovalNames.has(target.name)) {
        result.reason +=
          " Update the reviewed package.json allowScripts key in the same maintenance task.";
      }
      return {
        ecosystem: "npm",
        dependency: target.name,
        locked: target.locked,
        ...result,
      };
    } catch (error) {
      return errorRow("npm", target.name, target.locked, error);
    }
  });
  rows.push(...genericRows);

  const packages = isRecord(lockfile) && isRecord(lockfile.packages) ? lockfile.packages : null;
  if (packages === null)
    return [
      ...rows,
      errorRow(
        "npm / toolchain",
        "TypeScript aliases",
        "unknown",
        new MonitorDataError("package-lock.json has no packages map.")
      ),
    ];

  const toolchainRows = await mapWithConcurrency(typescriptTargets, 3, async (target) => {
    const lockEntry = packages[target.lockPath];
    if (
      !isRecord(lockEntry) ||
      lockEntry.name !== target.expectedPackageName ||
      parseStableVersion(lockEntry.version) === null
    ) {
      return errorRow(
        "npm / toolchain",
        target.dependency,
        "unknown",
        new MonitorDataError(
          `Expected ${target.lockPath} to contain ${target.expectedPackageName}.`
        )
      );
    }
    const locked = String(lockEntry.version);
    try {
      const metadata = await fetchNpmMetadata(target.registryPackage);
      const inspection = inspectNpmMetadata(metadata, target.registryPackage, target.line);
      const result = classifyNpmCurrency({
        locked,
        inspection,
        linePolicy: target.line === null ? null : { outsideLine: "ignore", reason: "" },
      });
      return {
        ecosystem: "npm / toolchain",
        dependency: target.dependency,
        locked,
        ...result,
      };
    } catch (error) {
      return errorRow("npm / toolchain", target.dependency, locked, error);
    }
  });
  rows.push(...toolchainRows);
  return rows;
}

/** @returns {Promise<string>} */
function runCargoMetadata() {
  return new Promise((resolvePromise, rejectPromise) => {
    execFile(
      "cargo",
      ["metadata", "--format-version", "1", "--locked", "--manifest-path", cargoManifestPath],
      {
        cwd: projectRoot,
        encoding: "utf8",
        maxBuffer: 64 * 1024 * 1024,
        timeout: 240_000,
        windowsHide: true,
      },
      (error, stdout) => {
        if (error !== null) {
          rejectPromise(
            new MonitorDataError(
              "cargo metadata failed; the Cargo update scope could not be inspected."
            )
          );
          return;
        }
        resolvePromise(stdout);
      }
    );
  });
}

/** @returns {Promise<CurrencyRow[]>} */
async function checkCargoDependencies() {
  let metadata;
  try {
    const text = await runCargoMetadata();
    metadata = JSON.parse(text);
  } catch (error) {
    return [errorRow("Cargo", "direct dependency graph", "unknown", error)];
  }

  let cargoScope;
  try {
    cargoScope = collectCargoTargets(metadata);
  } catch (error) {
    return [errorRow("Cargo", "direct dependency graph", "unknown", error)];
  }

  return await mapWithConcurrency(cargoScope.targets, 6, async (target) => {
    try {
      const indexPath = cratesIndexPath(target.name);
      const text = await cachedFetchText(
        `https://index.crates.io/${indexPath}`,
        cratesHeaders,
        8 * 1024 * 1024
      );
      const versions = parseCratesIndex(text, target.name);
      const policy = cargoPolicies[target.name] ?? {
        mode: /** @type {const} */ ("default"),
        reason: "",
        reviewedThrough: undefined,
      };
      const result = classifyCargoCurrency({
        locked: target.locked,
        versions,
        mode: policy.mode,
        policyReason: policy.reason,
        msrv: cargoScope.msrv,
        reviewedThrough: policy.reviewedThrough ?? null,
      });
      return {
        ecosystem: "Cargo",
        dependency: target.name,
        locked: target.locked,
        ...result,
      };
    } catch (error) {
      return errorRow("Cargo", target.name, target.locked, error);
    }
  });
}

/**
 * @param {string} directory
 * @returns {Promise<{path: string, content: string}[]>}
 */
async function readWorkflowSources(directory) {
  let entries;
  try {
    entries = await readdir(directory, { withFileTypes: true });
  } catch (error) {
    if (isRecord(error) && error.code === "ENOENT") return [];
    throw new MonitorDataError("Unable to read GitHub Actions workflow sources");
  }

  /** @type {{path: string, content: string}[]} */
  const files = [];
  for (const entry of entries) {
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await readWorkflowSources(path)));
      continue;
    }
    if (!entry.isFile() || (!entry.name.endsWith(".yml") && !entry.name.endsWith(".yaml")))
      continue;
    const content = await readFile(path, "utf8");
    if (Buffer.byteLength(content, "utf8") > 1024 * 1024) {
      throw new MonitorDataError("A GitHub Actions workflow source exceeds the 1 MiB safety cap");
    }
    files.push({ path: relative(projectRoot, path).split(sep).join("/"), content });
  }
  return files;
}

/**
 * @param {string} url
 * @returns {Promise<unknown>}
 */
async function fetchGitHubJson(url) {
  const text = await cachedFetchText(url, githubHeaders, 1024 * 1024);
  try {
    return JSON.parse(text);
  } catch {
    throw new MonitorDataError("GitHub returned malformed JSON for an action release");
  }
}

/**
 * @param {string} repository
 * @returns {Promise<unknown[]>}
 */
async function fetchGitHubReleases(repository) {
  /** @type {unknown[]} */
  const releases = [];
  for (let page = 1; page <= 3; page += 1) {
    const pageValue = await fetchGitHubJson(
      `https://api.github.com/repos/${repository}/releases?per_page=100&page=${page}`
    );
    if (!Array.isArray(pageValue) || pageValue.length > 100) {
      throw new MonitorDataError("GitHub returned an invalid action release page");
    }
    releases.push(...pageValue);
    if (pageValue.length < 100) return releases;
  }
  throw new MonitorDataError("GitHub action release history exceeds the 300-release safety cap");
}

/** @returns {Promise<CurrencyRow[]>} */
async function checkGitHubActions() {
  let parsed;
  try {
    const sources = (
      await Promise.all(workflowRoots.map((root) => readWorkflowSources(root)))
    ).flat();
    parsed = parseActionReferences(sources);
  } catch (error) {
    return [errorRow("GitHub Actions", "workflow sources", "unknown", error)];
  }

  /** @type {CurrencyRow[]} */
  const rows = parsed.errors.map((error) => {
    return errorRow(
      "GitHub Actions",
      error.dependency,
      "invalid ref",
      new MonitorDataError(error.reason)
    );
  });
  const actionRows = await mapWithConcurrency(parsed.actions, 4, async (action) => {
    try {
      const release = inspectGitHubReleases(await fetchGitHubReleases(action.repository));
      const declaredText = await cachedFetchText(
        `https://api.github.com/repos/${action.repository}/commits/${encodeURIComponent(`tags/${action.declaredTag}`)}`,
        { ...githubHeaders, Accept: "application/vnd.github.sha" },
        1024
      );
      const latestText =
        release.tag === action.declaredTag
          ? declaredText
          : await cachedFetchText(
              `https://api.github.com/repos/${action.repository}/commits/${encodeURIComponent(`tags/${release.tag}`)}`,
              { ...githubHeaders, Accept: "application/vnd.github.sha" },
              1024
            );
      const result = classifyActionCurrency({
        action,
        latestRelease: release,
        declaredSha: inspectResolvedCommitSha(declaredText),
        latestSha: inspectResolvedCommitSha(latestText),
      });
      return {
        ecosystem: "GitHub Actions",
        dependency: action.repository,
        locked: action.declaredTag,
        ...result,
      };
    } catch (error) {
      return errorRow("GitHub Actions", action.repository, action.declaredTag, error);
    }
  });
  rows.push(...actionRows);
  return rows;
}

/** @returns {Promise<void>} */
async function main() {
  /** @type {CurrencyRow[]} */
  let rows;
  try {
    rows = (
      await Promise.all([checkNpmDependencies(), checkCargoDependencies(), checkGitHubActions()])
    ).flat();
  } catch (error) {
    rows = [errorRow("Monitor", "orchestration", "unknown", error)];
  }

  let report;
  try {
    report = renderDependencyReport(rows);
  } catch (error) {
    rows = [errorRow("Monitor", "report generation", "unknown", error)];
    report = renderDependencyReport(rows);
  }

  console.log(report);
  const summaryPath = process.env.GITHUB_STEP_SUMMARY;
  if (typeof summaryPath === "string" && summaryPath.trim() !== "") {
    try {
      await appendFile(summaryPath, report, { encoding: "utf8" });
    } catch {
      console.log(
        "Monitor error: the GitHub job summary could not be written; the complete report is available above."
      );
      rows.push(
        errorRow(
          "Monitor",
          "GitHub job summary",
          "unknown",
          new MonitorDataError("Unable to write the GitHub job summary.")
        )
      );
    }
  }

  const exitCode = dependencyWatchExitCode(rows);
  if (exitCode === 1) {
    console.log("Dependency Watch found ordinary updates that require one human maintenance task.");
  } else if (exitCode === 2) {
    console.log(
      "Dependency Watch could not complete reliably; inspect the monitor-error rows above."
    );
  } else {
    console.log("Dependency Watch found no actionable ordinary updates.");
  }
  process.exitCode = exitCode;
}

await main();

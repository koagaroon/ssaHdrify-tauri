import { execFile } from "node:child_process";
import { lstat, readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { isDeepStrictEqual } from "node:util";

import {
  MonitorDataError,
  fetchTextWithPolicy,
  isRecord,
  sanitizeReportField,
} from "./dependency-currency.mjs";

const MAX_LOCAL_BYTES = 16 * 1024 * 1024;
const MAX_CHANGED_PATHS = 256;
const MAX_TAG_DEPTH = 8;
const SHA_PATTERN = /^[0-9a-f]{40}$/u;

const GRAPH_INPUTS = [
  "package.json",
  "package-lock.json",
  "src-tauri/Cargo.toml",
  "src-tauri/Cargo.lock",
  "src-tauri/sqlite-bundle.json",
  "src-tauri/vendor",
];
const COMPILER_INPUTS = ["rust-toolchain.toml"];
const DEV_ENV_INPUTS = [
  ".npmrc",
  ".nvmrc",
  ".node-version",
  ".cargo/config",
  ".cargo/config.toml",
  "src-tauri/.cargo/config",
  "src-tauri/.cargo/config.toml",
  ".github/workflows",
  ".github/actions",
];
const INPUTS = [...GRAPH_INPUTS, ...COMPILER_INPUTS, ...DEV_ENV_INPUTS];

const NPM_BUNDLER_ROOTS = new Set([
  "vite",
  "@vitejs/plugin-react",
  "@tailwindcss/vite",
  "tailwindcss",
  "esbuild",
]);

/** @typedef {{code: number, stdout: string}} GitResult */
/** @typedef {(args: string[]) => Promise<GitResult>} GitReader */
/** @typedef {(path: string) => Promise<string>} FileReader */
/** @typedef {"runtime" | "bundler" | "build"} InputImpactBucket */
/**
 * @typedef {object} ReleaseDependencyStatus
 * @property {"compared" | "no-release" | "error"} status
 * @property {string} repository
 * @property {string | null} sourceCommit
 * @property {boolean | null} sourceDirty
 * @property {string | null} releaseTag
 * @property {string | null} releaseTagObject
 * @property {string | null} releaseCommit
 * @property {"same" | "source-ahead" | "source-behind" | "diverged" | null} relationship
 * @property {{runtime: string[], bundler: string[], build: string[]}} changes
 * @property {string} reason
 */

/** @param {string} projectRoot @returns {GitReader} */
function localGitReader(projectRoot) {
  return (args) =>
    new Promise((resolvePromise, rejectPromise) => {
      execFile(
        "git",
        ["--no-optional-locks", "--no-replace-objects", "-c", "core.fsmonitor=false", ...args],
        {
          cwd: projectRoot,
          encoding: "utf8",
          timeout: 30_000,
          maxBuffer: MAX_LOCAL_BYTES,
          windowsHide: true,
          env: { ...process.env, GIT_NO_LAZY_FETCH: "1" },
        },
        (error, stdout) => {
          if (error !== null && typeof error.code !== "number") {
            rejectPromise(
              new MonitorDataError("Local Git inspection failed or exceeded its limit.")
            );
            return;
          }
          resolvePromise({ code: typeof error?.code === "number" ? error.code : 0, stdout });
        }
      );
    });
}

/** @type {FileReader} */
async function readLocalText(path) {
  const stat = await lstat(path);
  if (!stat.isFile() || stat.size > MAX_LOCAL_BYTES) {
    throw new MonitorDataError(
      "A dependency input is not a regular file or exceeds its size limit."
    );
  }
  const text = await readFile(path, "utf8");
  if (Buffer.byteLength(text, "utf8") > MAX_LOCAL_BYTES) {
    throw new MonitorDataError("A dependency input exceeds its size limit.");
  }
  return text;
}

/** @param {GitReader} runGit @param {string[]} args @param {string} reason */
async function gitResult(runGit, args, reason) {
  let result;
  try {
    result = await runGit(args);
  } catch {
    throw new MonitorDataError(reason);
  }
  if (
    !isRecord(result) ||
    !Number.isInteger(result.code) ||
    typeof result.code !== "number" ||
    result.code < 0 ||
    typeof result.stdout !== "string" ||
    Buffer.byteLength(result.stdout, "utf8") > MAX_LOCAL_BYTES
  ) {
    throw new MonitorDataError(reason);
  }
  return result;
}

/** @param {GitReader} runGit @param {string[]} args @param {string} reason */
async function gitText(runGit, args, reason) {
  const result = await gitResult(runGit, args, reason);
  if (result.code !== 0) throw new MonitorDataError(reason);
  return result.stdout;
}

/** @param {unknown} value */
function inspectObject(value) {
  if (
    !isRecord(value) ||
    (value.type !== "commit" && value.type !== "tag") ||
    typeof value.sha !== "string" ||
    !SHA_PATTERN.test(value.sha)
  ) {
    throw new MonitorDataError("GitHub returned an invalid release tag object.");
  }
  return { type: value.type, sha: value.sha };
}

/** @param {unknown} value */
function inspectRelease(value) {
  if (
    !isRecord(value) ||
    typeof value.draft !== "boolean" ||
    typeof value.prerelease !== "boolean" ||
    typeof value.tag_name !== "string" ||
    value.tag_name.length === 0 ||
    value.tag_name.length > 200 ||
    /\s/u.test(value.tag_name) ||
    Array.from(value.tag_name).some(
      (character) => character.charCodeAt(0) < 32 || character.charCodeAt(0) === 127
    ) ||
    (!value.draft &&
      (typeof value.published_at !== "string" || !Number.isFinite(Date.parse(value.published_at))))
  ) {
    throw new MonitorDataError("GitHub returned invalid published-release metadata.");
  }
  return { tag: value.tag_name, stable: !value.draft && !value.prerelease };
}

/**
 * @param {string} url
 * @param {Record<string, string>} headers
 * @param {typeof fetch} fetchImpl
 * @param {boolean} [allowMissing]
 */
async function githubJson(url, headers, fetchImpl, allowMissing = false) {
  let missing = false;
  const text = await fetchTextWithPolicy(url, {
    headers,
    maxBytes: 1024 * 1024,
    fetchImpl: async (input, options) => {
      const response = await fetchImpl(input, options);
      if (allowMissing && response.status === 404) {
        await response.body?.cancel();
        missing = true;
        // Absence is verified by a successful release listing, never by this 404 alone.
        return new Response("null", { status: 200 });
      }
      return response;
    },
  });
  if (missing) return { missing: true, value: null };
  try {
    return { missing: false, value: /** @type {unknown} */ (JSON.parse(text)) };
  } catch {
    throw new MonitorDataError("GitHub returned malformed JSON for the release comparison.");
  }
}

/** @param {string} text */
function changedPaths(text) {
  if (text === "") return [];
  if (!text.endsWith("\0"))
    throw new MonitorDataError("Local Git returned an invalid input listing.");
  const paths = text.slice(0, -1).split("\0");
  if (
    paths.length > MAX_CHANGED_PATHS ||
    paths.some(
      (path) =>
        path.length === 0 ||
        path.length > 1024 ||
        !INPUTS.some((input) => path === input || path.startsWith(`${input}/`)) ||
        path.split("/").some((part) => part === "." || part === "..")
    )
  ) {
    throw new MonitorDataError(
      "The changed dependency-input listing is invalid or exceeds its limit."
    );
  }
  return paths;
}

/** @param {string} text */
function parseComparedJson(text) {
  let value;
  try {
    value = JSON.parse(text);
  } catch {
    throw new MonitorDataError("A compared npm manifest or lockfile contains invalid JSON.");
  }
  if (!isRecord(value)) throw new MonitorDataError("A compared npm input is not a JSON object.");
  return value;
}

/**
 * @param {string} previousText
 * @param {string} currentText
 * @returns {InputImpactBucket}
 */
function classifyPackageJsonImpact(previousText, currentText) {
  const prev = parseComparedJson(previousText);
  const curr = parseComparedJson(currentText);
  const prevDeps = isRecord(prev.dependencies) ? prev.dependencies : {};
  const currDeps = isRecord(curr.dependencies) ? curr.dependencies : {};
  if (!isDeepStrictEqual(prevDeps, currDeps)) return "runtime";

  const prevDev = isRecord(prev.devDependencies) ? prev.devDependencies : {};
  const currDev = isRecord(curr.devDependencies) ? curr.devDependencies : {};
  for (const bundlerName of NPM_BUNDLER_ROOTS) {
    if (prevDev[bundlerName] !== currDev[bundlerName]) return "bundler";
  }
  return "build";
}

/**
 * Resolves a package key inside an npm v3 lockfile `packages` map using nested
 * `node_modules` hoisting rules.
 * @param {Record<string, unknown>} packages
 * @param {string} parentKey
 * @param {string} depName
 */
function resolveLockPackageKey(packages, parentKey, depName) {
  let scope = parentKey;
  while (scope !== "") {
    const candidate = `${scope}/node_modules/${depName}`;
    if (isRecord(packages[candidate])) return candidate;
    const nestedIndex = scope.lastIndexOf("/node_modules/");
    scope = nestedIndex >= 0 ? scope.slice(0, nestedIndex) : "";
  }
  const topCandidate = `node_modules/${depName}`;
  return isRecord(packages[topCandidate]) ? topCandidate : null;
}

/**
 * @param {Record<string, unknown>} packages
 * @param {Iterable<string>} rootNames
 */
function collectNpmLockClosure(packages, rootNames) {
  /** @type {Set<string>} */
  const visited = new Set();
  /** @type {string[]} */
  const queue = [];
  for (const rootName of rootNames) {
    const resolved = resolveLockPackageKey(packages, "", rootName);
    if (resolved !== null && !visited.has(resolved)) {
      visited.add(resolved);
      queue.push(resolved);
    }
  }
  while (queue.length > 0) {
    const pkgKey = queue.pop();
    if (pkgKey === undefined) continue;
    const entry = packages[pkgKey];
    if (!isRecord(entry)) continue;
    for (const section of ["dependencies", "optionalDependencies"]) {
      const deps = entry[section];
      if (!isRecord(deps)) continue;
      for (const depName of Object.keys(deps)) {
        const nextKey = resolveLockPackageKey(packages, pkgKey, depName);
        if (nextKey !== null && !visited.has(nextKey)) {
          visited.add(nextKey);
          queue.push(nextKey);
        }
      }
    }
  }
  return visited;
}

/**
 * @param {string} previousText
 * @param {string} currentText
 * @returns {InputImpactBucket}
 */
function classifyPackageLockImpact(previousText, currentText) {
  const prev = parseComparedJson(previousText);
  const curr = parseComparedJson(currentText);
  if (!isRecord(prev.packages) || !isRecord(curr.packages)) {
    throw new MonitorDataError("A compared package-lock.json has no packages map.");
  }
  const prevPkgs = prev.packages;
  const currPkgs = curr.packages;
  const prevBundlerKeys = collectNpmLockClosure(prevPkgs, NPM_BUNDLER_ROOTS);
  const currBundlerKeys = collectNpmLockClosure(currPkgs, NPM_BUNDLER_ROOTS);

  const allKeys = new Set([...Object.keys(prevPkgs), ...Object.keys(currPkgs)]);
  let hasBundlerChange = false;
  for (const key of allKeys) {
    if (key === "") continue;
    const prevEntry = prevPkgs[key];
    const currEntry = currPkgs[key];
    if (isDeepStrictEqual(prevEntry, currEntry)) continue;
    if (prevEntry !== undefined && !isRecord(prevEntry)) {
      throw new MonitorDataError("A compared package-lock.json entry is invalid.");
    }
    if (currEntry !== undefined && !isRecord(currEntry)) {
      throw new MonitorDataError("A compared package-lock.json entry is invalid.");
    }
    const prevDevOnly =
      prevEntry === undefined || prevEntry.dev === true || prevEntry.devOptional === true;
    const currDevOnly =
      currEntry === undefined || currEntry.dev === true || currEntry.devOptional === true;
    if (!prevDevOnly || !currDevOnly) {
      return "runtime";
    }
    if (prevBundlerKeys.has(key) || currBundlerKeys.has(key)) {
      hasBundlerChange = true;
    }
  }

  const prevRoot = isRecord(prevPkgs[""]) ? prevPkgs[""] : {};
  const currRoot = isRecord(currPkgs[""]) ? currPkgs[""] : {};
  const prevRootDeps = isRecord(prevRoot.dependencies) ? prevRoot.dependencies : {};
  const currRootDeps = isRecord(currRoot.dependencies) ? currRoot.dependencies : {};
  if (!isDeepStrictEqual(prevRootDeps, currRootDeps)) return "runtime";

  return hasBundlerChange ? "bundler" : "build";
}

/** @param {string} text */
function parseCargoTomlSections(text) {
  /** @type {Map<string, string[]>} */
  const sections = new Map();
  let currentHeader = "";
  let foundRustVersion = false;
  for (const rawLine of text.replace(/\r\n/gu, "\n").split("\n")) {
    const trimmed = rawLine.trim();
    if (trimmed === "" || trimmed.startsWith("#")) continue;
    const headerMatch = /^(?:\[\[([^\]]+)\]\]|\[([^\]]+)\])\s*(?:#.*)?$/u.exec(trimmed);
    if (headerMatch !== null) {
      currentHeader = headerMatch[1]
        ? `[[${headerMatch[1].trim()}]]`
        : `[${headerMatch[2].trim()}]`;
      if (!sections.has(currentHeader)) sections.set(currentHeader, []);
      continue;
    }
    if (currentHeader === "[package]") {
      if (/^rust-version\s*=\s*"\d+\.\d+(?:\.\d+)?"\s*(?:#.*)?$/u.test(trimmed)) {
        if (foundRustVersion) {
          throw new MonitorDataError("Cargo.toml repeats its Rust build requirement.");
        }
        foundRustVersion = true;
        continue;
      }
      if (/^version\s*=\s*"[^"]+"\s*(?:#.*)?$/u.test(trimmed)) {
        continue;
      }
    }
    const bucket = sections.get(currentHeader) ?? [];
    bucket.push(trimmed);
    sections.set(currentHeader, bucket);
  }
  return sections;
}

/**
 * @param {string} previousText
 * @param {string} currentText
 * @returns {InputImpactBucket}
 */
function classifyCargoTomlImpact(previousText, currentText) {
  const prev = parseCargoTomlSections(previousText);
  const curr = parseCargoTomlSections(currentText);
  const headers = new Set([...prev.keys(), ...curr.keys()]);
  let hasBundlerChange = false;
  for (const header of headers) {
    const prevLines = prev.get(header) ?? [];
    const currLines = curr.get(header) ?? [];
    if (isDeepStrictEqual(prevLines, currLines)) continue;
    if (header === "[dev-dependencies]" || header.startsWith("[dev-dependencies.")) {
      continue;
    }
    if (header === "[build-dependencies]" || header.startsWith("[build-dependencies.")) {
      hasBundlerChange = true;
      continue;
    }
    return "runtime";
  }
  return hasBundlerChange ? "bundler" : "build";
}

/** @param {string} cargoTomlText */
function extractCargoManifestEdges(cargoTomlText) {
  let currentHeader = "";
  let packageName = null;
  /** @type {Set<string>} */
  const runtimeDeps = new Set();
  /** @type {Set<string>} */
  const buildDeps = new Set();

  /**
   * @param {Set<string>} targetSet
   * @param {string} rawKey
   * @param {string} rawRhs
   */
  const recordDepName = (targetSet, rawKey, rawRhs) => {
    const packageOverride = /\bpackage\s*=\s*"([A-Za-z0-9_-]+)"/u.exec(rawRhs);
    targetSet.add(packageOverride ? packageOverride[1] : rawKey);
  };

  for (const rawLine of cargoTomlText.replace(/\r\n/gu, "\n").split("\n")) {
    const trimmed = rawLine.trim();
    if (trimmed === "" || trimmed.startsWith("#")) continue;
    const tableMatch = /^\[(build-dependencies|dependencies)\.([A-Za-z0-9_-]+)\]\s*(?:#.*)?$/u.exec(
      trimmed
    );
    if (tableMatch !== null) {
      currentHeader = `table:${tableMatch[1]}:${tableMatch[2]}`;
      (tableMatch[1] === "dependencies" ? runtimeDeps : buildDeps).add(tableMatch[2]);
      continue;
    }
    const headerMatch = /^\[([^\]]+)\]\s*(?:#.*)?$/u.exec(trimmed);
    if (headerMatch !== null) {
      currentHeader = `[${headerMatch[1].trim()}]`;
      continue;
    }
    if (currentHeader === "[package]") {
      const nameMatch = /^name\s*=\s*"([A-Za-z0-9_-]+)"/u.exec(trimmed);
      if (nameMatch !== null) packageName = nameMatch[1];
      continue;
    }
    if (currentHeader.startsWith("table:")) {
      const [, kind, keyName] = currentHeader.split(":");
      const pkgMatch = /^package\s*=\s*"([A-Za-z0-9_-]+)"/u.exec(trimmed);
      if (pkgMatch !== null) {
        const set = kind === "dependencies" ? runtimeDeps : buildDeps;
        set.delete(keyName);
        set.add(pkgMatch[1]);
      }
      continue;
    }
    if (currentHeader === "[dependencies]" || currentHeader === "[build-dependencies]") {
      const kvMatch = /^([A-Za-z0-9_-]+)\s*=\s*(.+)$/u.exec(trimmed);
      if (kvMatch !== null) {
        recordDepName(
          currentHeader === "[dependencies]" ? runtimeDeps : buildDeps,
          kvMatch[1],
          kvMatch[2]
        );
      }
    }
  }
  return packageName === null ? null : { packageName, runtimeDeps, buildDeps };
}

/** @param {string} lockText */
function parseCargoLockPackages(lockText) {
  const blocks = lockText.replace(/\r\n/gu, "\n").split(/\n\[\[package\]\]\n/u);
  /** @type {Array<{name: string, version: string, source: string | null, checksum: string | null, deps: string[]}>} */
  const packages = [];
  for (const [index, block] of blocks.entries()) {
    if (index === 0 && !block.trimStart().startsWith("[[package]]")) continue;
    const cleanBlock = block.replace(/^\s*\[\[package\]\]\s*\n/u, "");
    const name = /^name\s*=\s*"([^"]+)"$/mu.exec(cleanBlock)?.[1] ?? null;
    const version = /^version\s*=\s*"([^"]+)"$/mu.exec(cleanBlock)?.[1] ?? null;
    if (name === null || version === null) {
      throw new MonitorDataError("A compared Cargo.lock contains an invalid [[package]] block.");
    }
    const source = /^source\s*=\s*"([^"]+)"$/mu.exec(cleanBlock)?.[1] ?? null;
    const checksum = /^checksum\s*=\s*"([^"]+)"$/mu.exec(cleanBlock)?.[1] ?? null;
    const depsMatch = /^dependencies\s*=\s*\[([\s\S]*?)^\]/mu.exec(cleanBlock);
    const deps = depsMatch
      ? [...depsMatch[1].matchAll(/"([^"]+)"/gu)].map((match) => match[1])
      : [];
    packages.push({ name, version, source, checksum, deps });
  }
  if (packages.length === 0) {
    throw new MonitorDataError("A compared Cargo.lock contains no [[package]] entries.");
  }
  return packages;
}

/**
 * @param {ReturnType<typeof parseCargoLockPackages>} packages
 * @param {string} workspaceName
 * @param {Set<string>} rootDepNames
 */
function collectCargoLockClosure(packages, workspaceName, rootDepNames) {
  const rootPkg = packages.find((pkg) => pkg.name === workspaceName && pkg.source === null);
  if (rootPkg === undefined) return null;

  /** @param {string} spec */
  const resolveSpec = (spec) => {
    const parts = spec.split(" ");
    const depName = parts[0];
    const depVer = parts[1] ?? null;
    return packages.filter(
      (pkg) => pkg.name === depName && (depVer === null || pkg.version === depVer)
    );
  };

  /** @type {Map<string, {version: string, source: string | null, checksum: string | null}>} */
  const closure = new Map();
  /** @type {typeof packages} */
  const queue = [];
  for (const depSpec of rootPkg.deps) {
    const depName = depSpec.split(" ")[0];
    if (!rootDepNames.has(depName)) continue;
    for (const target of resolveSpec(depSpec)) {
      const id = `${target.name}@${target.version}`;
      if (!closure.has(id)) {
        closure.set(id, {
          version: target.version,
          source: target.source,
          checksum: target.checksum,
        });
        queue.push(target);
      }
    }
  }
  while (queue.length > 0) {
    const current = queue.pop();
    if (current === undefined) continue;
    for (const depSpec of current.deps) {
      for (const target of resolveSpec(depSpec)) {
        const id = `${target.name}@${target.version}`;
        if (!closure.has(id)) {
          closure.set(id, {
            version: target.version,
            source: target.source,
            checksum: target.checksum,
          });
          queue.push(target);
        }
      }
    }
  }
  return Object.fromEntries([...closure.entries()].sort(([a], [b]) => a.localeCompare(b)));
}

/**
 * @param {object} options
 * @param {string} options.path
 * @param {string} options.releaseCommit
 * @param {string} options.projectRoot
 * @param {GitReader} options.runGit
 * @param {FileReader} options.readFileImpl
 * @returns {Promise<InputImpactBucket>}
 */
async function classifyChangedInput({ path, releaseCommit, projectRoot, runGit, readFileImpl }) {
  if (COMPILER_INPUTS.some((input) => path === input || path.startsWith(`${input}/`))) {
    return "bundler";
  }
  if (DEV_ENV_INPUTS.some((input) => path === input || path.startsWith(`${input}/`))) {
    return "build";
  }
  if (path === "src-tauri/sqlite-bundle.json" || path.startsWith("src-tauri/vendor/")) {
    return "runtime";
  }

  const entry = await gitText(
    runGit,
    ["ls-tree", "-z", releaseCommit, "--", path],
    "The release input tree could not be inspected."
  );
  if (entry === "") return "runtime";
  const previous = await gitText(
    runGit,
    ["show", `${releaseCommit}:${path}`],
    "A released dependency input is missing from the local Git objects."
  );
  let current;
  try {
    current = await readFileImpl(resolve(projectRoot, path));
  } catch (error) {
    if (isRecord(error) && error.code === "ENOENT") return "runtime";
    throw new MonitorDataError("A current dependency input could not be read safely.");
  }
  if (typeof current !== "string" || Buffer.byteLength(current, "utf8") > MAX_LOCAL_BYTES) {
    throw new MonitorDataError("A current dependency input exceeds its size limit.");
  }

  if (path === "package.json") return classifyPackageJsonImpact(previous, current);
  if (path === "package-lock.json") return classifyPackageLockImpact(previous, current);
  if (path === "src-tauri/Cargo.toml") return classifyCargoTomlImpact(previous, current);
  if (path === "src-tauri/Cargo.lock") {
    const prevPackages = parseCargoLockPackages(previous);
    const currPackages = parseCargoLockPackages(current);
    let prevCargoToml = null;
    let currCargoToml;
    try {
      const tomlTree = await gitText(
        runGit,
        ["ls-tree", "-z", releaseCommit, "--", "src-tauri/Cargo.toml"],
        "The release Cargo.toml tree could not be inspected."
      );
      if (tomlTree !== "") {
        prevCargoToml = await gitText(
          runGit,
          ["show", `${releaseCommit}:src-tauri/Cargo.toml`],
          "Released Cargo.toml could not be read."
        );
      }
      currCargoToml = await readFileImpl(resolve(projectRoot, "src-tauri/Cargo.toml"));
    } catch {
      return "runtime";
    }
    if (prevCargoToml === null || typeof currCargoToml !== "string") return "runtime";
    const prevEdges = extractCargoManifestEdges(prevCargoToml);
    const currEdges = extractCargoManifestEdges(currCargoToml);
    if (prevEdges === null || currEdges === null) return "runtime";

    const prevRuntime = collectCargoLockClosure(
      prevPackages,
      prevEdges.packageName,
      prevEdges.runtimeDeps
    );
    const currRuntime = collectCargoLockClosure(
      currPackages,
      currEdges.packageName,
      currEdges.runtimeDeps
    );
    if (prevRuntime === null || currRuntime === null) return "runtime";
    if (!isDeepStrictEqual(prevRuntime, currRuntime)) return "runtime";

    const prevBuild = collectCargoLockClosure(
      prevPackages,
      prevEdges.packageName,
      prevEdges.buildDeps
    );
    const currBuild = collectCargoLockClosure(
      currPackages,
      currEdges.packageName,
      currEdges.buildDeps
    );
    if (prevBuild === null || currBuild === null) return "runtime";
    return isDeepStrictEqual(prevBuild, currBuild) ? "build" : "bundler";
  }

  return "runtime";
}

/**
 * Compares selected source inputs with GitHub's latest published stable release.
 * @param {object} options
 * @param {string} options.projectRoot
 * @param {string} [options.repository]
 * @param {Record<string, string>} [options.headers]
 * @param {typeof fetch} [options.fetchImpl]
 * @param {GitReader} [options.runGit]
 * @param {FileReader} [options.readFileImpl]
 * @returns {Promise<ReleaseDependencyStatus>}
 */
export async function inspectReleaseDependencyStatus({
  projectRoot,
  repository = "koagaroon/ssaHdrify-tauri",
  headers = {},
  fetchImpl = fetch,
  runGit = localGitReader(projectRoot),
  readFileImpl = readLocalText,
}) {
  /** @type {ReleaseDependencyStatus} */
  const result = {
    status: "error",
    repository,
    sourceCommit: null,
    sourceDirty: null,
    releaseTag: null,
    releaseTagObject: null,
    releaseCommit: null,
    relationship: null,
    changes: { runtime: [], bundler: [], build: [] },
    reason: "The published-release comparison could not be completed.",
  };
  try {
    if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/u.test(repository) || repository.length > 200) {
      throw new MonitorDataError("The release repository identifier is invalid.");
    }
    const sourceCommit = (
      await gitText(
        runGit,
        ["rev-parse", "--verify", "HEAD^{commit}"],
        "Source HEAD is unavailable."
      )
    ).trim();
    if (!SHA_PATTERN.test(sourceCommit))
      throw new MonitorDataError("Source HEAD is not a full commit.");
    result.sourceCommit = sourceCommit;
    const statusArgs = [
      "status",
      "--porcelain=v1",
      "-z",
      "--untracked-files=all",
      "--ignore-submodules=none",
    ];
    const sourceStatus = await gitText(
      runGit,
      statusArgs,
      "The source working tree could not be inspected."
    );
    result.sourceDirty = sourceStatus !== "";

    const api = `https://api.github.com/repos/${repository}`;
    const latest = await githubJson(`${api}/releases/latest`, headers, fetchImpl, true);
    if (latest.missing) {
      for (let page = 1; page <= 3; page += 1) {
        const { value } = await githubJson(
          `${api}/releases?per_page=100&page=${page}`,
          headers,
          fetchImpl
        );
        if (!Array.isArray(value) || value.length > 100) {
          throw new MonitorDataError("GitHub returned an invalid release listing.");
        }
        if (value.some((release) => inspectRelease(release).stable)) {
          throw new MonitorDataError(
            "GitHub's latest-release lookup disagrees with its published-release listing."
          );
        }
        if (value.length < 100) {
          result.status = "no-release";
          result.reason =
            "A successful release listing contains no published stable release; no release baseline is available.";
          return result;
        }
      }
      throw new MonitorDataError(
        "Release absence cannot be verified within the 300-release inspection limit."
      );
    }
    const release = inspectRelease(latest.value);
    if (!release.stable)
      throw new MonitorDataError("GitHub's latest release is not a published stable release.");
    result.releaseTag = release.tag;
    const { value: ref } = await githubJson(
      `${api}/git/ref/tags/${encodeURIComponent(release.tag)}`,
      headers,
      fetchImpl
    );
    if (!isRecord(ref) || ref.ref !== `refs/tags/${release.tag}`) {
      throw new MonitorDataError(
        "GitHub did not return the exact published-release tag reference."
      );
    }
    let object = inspectObject(ref.object);
    result.releaseTagObject = object.sha;
    const seen = new Set();
    while (object.type === "tag") {
      if (seen.size >= MAX_TAG_DEPTH || seen.has(object.sha)) {
        throw new MonitorDataError(
          "The release tag chain is cyclic or exceeds the eight-tag inspection limit."
        );
      }
      seen.add(object.sha);
      const { value: tag } = await githubJson(`${api}/git/tags/${object.sha}`, headers, fetchImpl);
      if (!isRecord(tag) || tag.sha !== object.sha) {
        throw new MonitorDataError("GitHub returned a mismatched annotated release tag.");
      }
      object = inspectObject(tag.object);
    }
    result.releaseCommit = object.sha;
    const localType = await gitText(
      runGit,
      ["cat-file", "-t", object.sha],
      "The released commit is unavailable locally; refresh full Git history outside this monitor."
    );
    if (localType.trim() !== "commit")
      throw new MonitorDataError("The local released object is not a commit.");
    if (sourceCommit === object.sha) {
      result.relationship = "same";
    } else {
      const shallow = await gitText(
        runGit,
        ["rev-parse", "--is-shallow-repository"],
        "Git history completeness could not be inspected."
      );
      if (shallow.trim() !== "false")
        throw new MonitorDataError(
          "Source ancestry requires a complete local Git history; this monitor does not fetch it."
        );
      const forward = await gitResult(
        runGit,
        ["merge-base", "--is-ancestor", object.sha, sourceCommit],
        "Release-to-source ancestry could not be inspected."
      );
      if (forward.code > 1)
        throw new MonitorDataError("Release-to-source ancestry could not be inspected.");
      if (forward.code === 0) result.relationship = "source-ahead";
      else {
        const backward = await gitResult(
          runGit,
          ["merge-base", "--is-ancestor", sourceCommit, object.sha],
          "Source-to-release ancestry could not be inspected."
        );
        if (backward.code > 1)
          throw new MonitorDataError("Source-to-release ancestry could not be inspected.");
        result.relationship = backward.code === 0 ? "source-behind" : "diverged";
      }
    }
    const changed = changedPaths(
      await gitText(
        runGit,
        [
          "diff",
          "--name-only",
          "-z",
          "--no-renames",
          "--no-ext-diff",
          "--no-textconv",
          object.sha,
          "--",
          ...INPUTS,
        ],
        "Released and current dependency inputs could not be compared."
      )
    );
    const untracked = changedPaths(
      await gitText(
        runGit,
        ["ls-files", "--others", "--exclude-standard", "-z", "--", ...INPUTS],
        "Untracked dependency inputs could not be inspected."
      )
    );
    const paths = [...new Set([...changed, ...untracked])].sort();
    if (paths.length > MAX_CHANGED_PATHS)
      throw new MonitorDataError(
        "The changed dependency inputs exceed the 256-file inspection limit."
      );
    for (const path of paths) {
      const bucket = untracked.includes(path)
        ? COMPILER_INPUTS.some((input) => path === input || path.startsWith(`${input}/`))
          ? "bundler"
          : DEV_ENV_INPUTS.some((input) => path === input || path.startsWith(`${input}/`))
            ? "build"
            : "runtime"
        : await classifyChangedInput({
            path,
            releaseCommit: object.sha,
            projectRoot,
            runGit,
            readFileImpl,
          });
      result.changes[bucket].push(path);
    }
    const endHead = await gitText(
      runGit,
      ["rev-parse", "--verify", "HEAD^{commit}"],
      "Source HEAD could not be rechecked."
    );
    const endStatus = await gitText(
      runGit,
      statusArgs,
      "The source working tree could not be rechecked."
    );
    if (endHead.trim() !== sourceCommit || endStatus !== sourceStatus) {
      throw new MonitorDataError(
        "The source checkout changed during comparison; rerun after local changes settle."
      );
    }
    result.status = "compared";
    if (paths.length === 0) {
      result.reason = "Selected dependency and build inputs match the released source.";
    } else if (result.changes.runtime.length === 0 && result.changes.bundler.length === 0) {
      result.reason =
        "Only dev/CI or build-environment inputs differ from the released source; resolved shipped runtime dependencies match.";
    } else if (result.changes.runtime.length === 0) {
      result.reason =
        "Bundler/compiler or dev/CI inputs differ from the released source; resolved shipped runtime package versions match.";
    } else {
      result.reason =
        "Shipped runtime dependency inputs differ from the released source; the differences are informational.";
    }
    return result;
  } catch (error) {
    result.reason =
      error instanceof MonitorDataError
        ? error.message
        : "The published-release comparison encountered an unexpected failure.";
    return result;
  }
}

/** @param {ReleaseDependencyStatus} result */
export function releaseDependencyExitCode(result) {
  return result.status === "error" ? 2 : 0;
}

/** @param {unknown} value */
function reportField(value) {
  return sanitizeReportField(String(value).replace(/[\p{Cc}\p{Cf}]/gu, " "));
}

/** @param {ReleaseDependencyStatus} result */
export function renderReleaseDependencyReport(result) {
  const changed = [...result.changes.runtime, ...result.changes.bundler, ...result.changes.build];
  if (changed.length > MAX_CHANGED_PATHS)
    throw new MonitorDataError("The release comparison report exceeds its file limit.");
  const relationship = {
    same: "The source HEAD is the release commit.",
    "source-ahead": "The released commit is an ancestor of source HEAD.",
    "source-behind": "Source HEAD is an ancestor of the released commit.",
    diverged: "Source HEAD and the released commit have diverged histories.",
  };
  const dirty =
    result.sourceDirty === null
      ? "working-tree state unknown"
      : result.sourceDirty
        ? "dirty working tree; current files include local changes"
        : "clean working tree";
  const lines = [
    "## Published release comparison",
    "",
    `Source HEAD: ${reportField(result.sourceCommit ?? "unknown")} (${dirty}).`,
    `Latest published stable release: ${reportField(result.releaseTag ?? "unavailable")}.`,
    `Release tag object: ${reportField(result.releaseTagObject ?? "unavailable")}.`,
    `Released source commit: ${reportField(result.releaseCommit ?? "unavailable")}.`,
    "",
    `Comparison status: ${result.status === "error" ? "monitor incomplete" : result.status === "no-release" ? "no stable release baseline" : "complete"}. ${reportField(result.reason)}`,
    result.relationship === null ? "" : relationship[result.relationship],
    "",
  ];
  if (result.status === "compared") {
    lines.push(
      `Shipped runtime inputs changed: ${result.changes.runtime.length}. Build-output/compiler inputs changed: ${result.changes.bundler.length}. Dev/CI-only inputs changed: ${result.changes.build.length}.`,
      ""
    );
    if (changed.length > 0) {
      lines.push("| Binary impact | Changed file |", "| --- | --- |");
      for (const [group, paths] of [
        ["Shipped in exe (runtime)", result.changes.runtime],
        ["Build output / compiler", result.changes.bundler],
        ["Dev / CI / workflow only", result.changes.build],
      ]) {
        for (const path of paths) lines.push(`| ${group} | ${reportField(path)} |`);
      }
      lines.push("");
    }
  }
  lines.push(
    "Scope: npm and Cargo manifests/lockfiles, SQLite bundle provenance, historical vendored source, build-tool configuration, and GitHub workflows/actions. Lockfile comparisons walk resolved runtime vs. bundler vs. dev-only package closures so dev-tool updates are separated from shipped `.exe` dependency updates.",
    "",
    "This compares selected source inputs, including visible uncommitted and untracked inputs. It does not inspect release binaries, establish shipped security status or release readiness, or decide whether to publish. Application source outside this scope is not compared. Input differences alone do not make this monitor fail.",
    ""
  );
  const report = lines.join("\n");
  if (Buffer.byteLength(report, "utf8") > 96 * 1024)
    throw new MonitorDataError("The release comparison report exceeds its size limit.");
  return report;
}

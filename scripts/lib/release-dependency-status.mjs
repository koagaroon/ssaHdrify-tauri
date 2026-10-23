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
  "src-tauri/vendor",
];
const BUILD_INPUTS = [
  "rust-toolchain.toml",
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
const INPUTS = [...GRAPH_INPUTS, ...BUILD_INPUTS];

/** @typedef {{code: number, stdout: string}} GitResult */
/** @typedef {(args: string[]) => Promise<GitResult>} GitReader */
/** @typedef {(path: string) => Promise<string>} FileReader */
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
 * @property {{graph: string[], build: string[]}} changes
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

/** @param {string} text @param {string} path */
function withoutBuildMetadata(text, path) {
  if (path === "src-tauri/Cargo.toml") {
    let inPackage = false;
    let found = false;
    const lines = text.replace(/\r\n/gu, "\n").split("\n");
    const remaining = lines.filter((line) => {
      if (/^\s*\[/u.test(line)) inPackage = /^\s*\[package\]\s*(?:#.*)?$/u.test(line);
      if (inPackage && /^\s*rust-version\s*=\s*"\d+\.\d+(?:\.\d+)?"\s*(?:#.*)?$/u.test(line)) {
        if (found) throw new MonitorDataError("Cargo.toml repeats its Rust build requirement.");
        found = true;
        return false;
      }
      return true;
    });
    return found ? remaining.join("\n") : text.replace(/\r\n/gu, "\n");
  }
  let value;
  try {
    value = JSON.parse(text);
  } catch {
    throw new MonitorDataError("A compared npm manifest or lockfile contains invalid JSON.");
  }
  if (!isRecord(value)) throw new MonitorDataError("A compared npm input is not a JSON object.");
  if (path === "package.json") {
    delete value.engines;
    delete value.packageManager;
    delete value.devEngines;
  } else if (isRecord(value.packages) && isRecord(value.packages[""])) {
    delete value.packages[""].engines;
    delete value.packages[""].devEngines;
  }
  return value;
}

/**
 * @param {string} path
 * @param {string} releaseCommit
 * @param {string} projectRoot
 * @param {GitReader} runGit
 * @param {FileReader} readFileImpl
 */
async function isBuildMetadataOnly(path, releaseCommit, projectRoot, runGit, readFileImpl) {
  if (!["package.json", "package-lock.json", "src-tauri/Cargo.toml"].includes(path)) return false;
  const entry = await gitText(
    runGit,
    ["ls-tree", "-z", releaseCommit, "--", path],
    "The release input tree could not be inspected."
  );
  if (entry === "") return false;
  const previous = await gitText(
    runGit,
    ["show", `${releaseCommit}:${path}`],
    "A released dependency input is missing from the local Git objects."
  );
  let current;
  try {
    current = await readFileImpl(resolve(projectRoot, path));
  } catch (error) {
    if (isRecord(error) && error.code === "ENOENT") return false;
    throw new MonitorDataError("A current dependency input could not be read safely.");
  }
  if (typeof current !== "string" || Buffer.byteLength(current, "utf8") > MAX_LOCAL_BYTES) {
    throw new MonitorDataError("A current dependency input exceeds its size limit.");
  }
  return isDeepStrictEqual(
    withoutBuildMetadata(previous, path),
    withoutBuildMetadata(current, path)
  );
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
    changes: { graph: [], build: [] },
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
      const buildOnly =
        BUILD_INPUTS.some((input) => path === input || path.startsWith(`${input}/`)) ||
        (await isBuildMetadataOnly(path, object.sha, projectRoot, runGit, readFileImpl));
      result.changes[buildOnly ? "build" : "graph"].push(path);
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
    result.reason =
      paths.length === 0
        ? "Selected dependency and build inputs match the released source."
        : "Selected source inputs differ from the released source; the differences are informational.";
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
  const changed = [...result.changes.graph, ...result.changes.build];
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
      `Changed mixed dependency inputs: ${result.changes.graph.length}. Changed build-only inputs: ${result.changes.build.length}.`,
      ""
    );
    if (changed.length > 0) {
      lines.push("| Input group | Changed file |", "| --- | --- |");
      for (const [group, paths] of [
        ["Mixed runtime/build dependency inputs", result.changes.graph],
        ["Build environment / workflows", result.changes.build],
      ]) {
        for (const path of paths) lines.push(`| ${group} | ${reportField(path)} |`);
      }
      lines.push("");
    }
  }
  lines.push(
    "Scope: npm and Cargo manifests/lockfiles, vendored source, build-tool configuration, and GitHub workflows/actions. Engine/package-manager metadata alone and Cargo's package rust-version alone are classified as build inputs. Other manifest/lockfile differences remain mixed inputs and do not prove a runtime dependency changed.",
    "",
    "This compares selected source inputs, including visible uncommitted and untracked inputs. It does not inspect release binaries, establish shipped security status or release readiness, or decide whether to publish. Application source outside this scope is not compared. Input differences alone do not make this monitor fail.",
    ""
  );
  const report = lines.join("\n");
  if (Buffer.byteLength(report, "utf8") > 96 * 1024)
    throw new MonitorDataError("The release comparison report exceeds its size limit.");
  return report;
}

const MAX_VERSION_LENGTH = 128;
const MAX_REPORT_FIELD_LENGTH = 240;
const MAX_REPORT_ROWS = 250;
const MAX_SUMMARY_BYTES = 900_000;

const SEMVER_PATTERN =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/u;
const RUST_VERSION_PATTERN = /^(0|[1-9]\d*)(?:\.(0|[1-9]\d*))?(?:\.(0|[1-9]\d*))?$/u;
const NPM_PACKAGE_PATTERN = /^(?:@[a-z0-9._~-]+\/)?[a-z0-9._~-]+$/u;
const CRATE_NAME_PATTERN = /^[A-Za-z0-9_-]{1,64}$/u;
const ACTION_COMPONENT_PATTERN = /^[A-Za-z0-9_.-]+$/u;
const FULL_COMMIT_SHA_PATTERN = /^[0-9a-f]{40}$/iu;
const CRATES_IO_SOURCE_IDS = new Set([
  "registry+https://github.com/rust-lang/crates.io-index",
  "sparse+https://index.crates.io/",
]);

/** @typedef {"current" | "update" | "review" | "held" | "error"} CurrencyStatus */

/**
 * @typedef {object} ParsedSemver
 * @property {string} raw
 * @property {number} major
 * @property {number} minor
 * @property {number} patch
 * @property {string | null} prerelease
 */

/**
 * @typedef {object} CurrencyRow
 * @property {string} ecosystem
 * @property {string} dependency
 * @property {string} locked
 * @property {string} latest
 * @property {CurrencyStatus} status
 * @property {string} reason
 */

/**
 * @typedef {object} NpmTarget
 * @property {string} name
 * @property {string} requested
 * @property {string} locked
 * @property {"dependency" | "development"} kind
 */

/**
 * @typedef {object} CrateVersion
 * @property {string} version
 * @property {boolean} yanked
 * @property {string | null} rustVersion
 */

/**
 * @typedef {object} CargoTarget
 * @property {string} name
 * @property {string} locked
 * @property {string[]} kinds
 */

/**
 * @typedef {object} ActionTarget
 * @property {string} repository
 * @property {string} pin
 * @property {string} declaredTag
 * @property {string[]} locations
 */

export class MonitorDataError extends Error {
  /** @param {string} message */
  constructor(message) {
    super(message);
    this.name = "MonitorDataError";
  }
}

/**
 * @param {unknown} value
 * @returns {value is Record<string, unknown>}
 */
export function isRecord(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** @param {unknown} value */
export function parseSemver(value) {
  if (typeof value !== "string" || value.length === 0 || value.length > MAX_VERSION_LENGTH) {
    return null;
  }

  const match = SEMVER_PATTERN.exec(value);
  if (match === null) return null;

  const numericParts = match.slice(1, 4).map(Number);
  if (numericParts.some((part) => !Number.isSafeInteger(part))) return null;

  return {
    raw: value,
    major: numericParts[0],
    minor: numericParts[1],
    patch: numericParts[2],
    prerelease: match[4] ?? null,
  };
}

/** @param {unknown} value */
export function parseStableVersion(value) {
  const parsed = parseSemver(value);
  return parsed !== null && parsed.prerelease === null ? parsed : null;
}

/**
 * @param {ParsedSemver} left
 * @param {ParsedSemver} right
 */
export function compareStableVersions(left, right) {
  for (const key of /** @type {const} */ (["major", "minor", "patch"])) {
    if (left[key] !== right[key]) return left[key] < right[key] ? -1 : 1;
  }
  return 0;
}

/**
 * @param {Iterable<string>} versions
 * @param {(version: ParsedSemver) => boolean} [predicate]
 */
export function maxStableVersion(versions, predicate = () => true) {
  /** @type {ParsedSemver | null} */
  let greatest = null;

  for (const value of versions) {
    const parsed = parseStableVersion(value);
    if (parsed === null || !predicate(parsed)) continue;
    if (greatest === null || compareStableVersions(parsed, greatest) > 0) greatest = parsed;
  }

  return greatest?.raw;
}

/** @param {unknown} value */
export function parseRustVersion(value) {
  if (typeof value !== "string" || value.length > MAX_VERSION_LENGTH) return null;
  const match = RUST_VERSION_PATTERN.exec(value);
  if (match === null) return null;
  const parts = [match[1], match[2] ?? "0", match[3] ?? "0"].map(Number);
  if (parts.some((part) => !Number.isSafeInteger(part))) return null;
  return { major: parts[0], minor: parts[1], patch: parts[2] };
}

/**
 * @param {{major: number, minor: number, patch: number}} left
 * @param {{major: number, minor: number, patch: number}} right
 */
export function compareRustVersions(left, right) {
  for (const key of /** @type {const} */ (["major", "minor", "patch"])) {
    if (left[key] !== right[key]) return left[key] < right[key] ? -1 : 1;
  }
  return 0;
}

/**
 * @param {unknown} packageJson
 * @param {unknown} lockfile
 */
export function collectNpmTargets(packageJson, lockfile) {
  if (!isRecord(packageJson) || !isRecord(lockfile) || !isRecord(lockfile.packages)) {
    throw new MonitorDataError("package.json or package-lock.json has an invalid root shape");
  }

  const lockRoot = lockfile.packages[""];
  if (!isRecord(lockRoot)) {
    throw new MonitorDataError('package-lock.json is missing its packages[""] root entry');
  }

  /** @type {NpmTarget[]} */
  const targets = [];
  /** @type {string[]} */
  const aliases = [];
  const seen = new Set();

  for (const [section, kind] of /** @type {const} */ ([
    ["dependencies", "dependency"],
    ["devDependencies", "development"],
  ])) {
    const declared = packageJson[section];
    const lockedRootSection = lockRoot[section];
    if (!isRecord(declared) || !isRecord(lockedRootSection)) {
      throw new MonitorDataError(`${section} is missing from package.json or package-lock.json`);
    }

    for (const [name, requested] of Object.entries(declared)) {
      if (!NPM_PACKAGE_PATTERN.test(name) || typeof requested !== "string") {
        throw new MonitorDataError(`Invalid direct npm dependency declaration in ${section}`);
      }
      if (seen.has(name)) throw new MonitorDataError(`Direct npm dependency ${name} is duplicated`);
      seen.add(name);

      if (lockedRootSection[name] !== requested) {
        throw new MonitorDataError(`package-lock.json does not match ${section}.${name}`);
      }

      if (requested.startsWith("npm:")) {
        aliases.push(name);
        continue;
      }

      const lockEntry = lockfile.packages[`node_modules/${name}`];
      if (!isRecord(lockEntry) || parseStableVersion(lockEntry.version) === null) {
        throw new MonitorDataError(`package-lock.json has no stable locked version for ${name}`);
      }

      targets.push({ name, requested, locked: String(lockEntry.version), kind });
    }
  }

  return { targets, aliases };
}

/**
 * @param {unknown} metadata
 * @param {string} expectedName
 * @param {{major: number, minor?: number} | null} [line]
 */
export function inspectNpmMetadata(metadata, expectedName, line = null) {
  if (!NPM_PACKAGE_PATTERN.test(expectedName) || !isRecord(metadata)) {
    throw new MonitorDataError(`npm returned invalid metadata for ${expectedName}`);
  }
  if (metadata.name !== expectedName || !isRecord(metadata["dist-tags"])) {
    throw new MonitorDataError(`npm metadata identity mismatch for ${expectedName}`);
  }

  const publisherLatest = metadata["dist-tags"].latest;
  if (typeof publisherLatest !== "string" || parseSemver(publisherLatest) === null) {
    throw new MonitorDataError(`npm returned an invalid latest tag for ${expectedName}`);
  }
  if (!isRecord(metadata.versions) || Object.keys(metadata.versions).length > 100_000) {
    throw new MonitorDataError(`npm returned an invalid version map for ${expectedName}`);
  }

  const latestRecord = metadata.versions[publisherLatest];
  if (!isRecord(latestRecord) || latestRecord.version !== publisherLatest) {
    throw new MonitorDataError(`npm latest metadata is incomplete for ${expectedName}`);
  }

  const stableVersions = Object.keys(metadata.versions).filter(
    (version) => parseStableVersion(version) !== null
  );
  const globalStable = maxStableVersion(stableVersions);
  if (globalStable === undefined) {
    throw new MonitorDataError(`npm returned no stable versions for ${expectedName}`);
  }

  const trackedLatest =
    line === null
      ? parseStableVersion(publisherLatest) === null
        ? undefined
        : publisherLatest
      : maxStableVersion(stableVersions, (version) => {
          return (
            version.major === line.major &&
            (line.minor === undefined || version.minor === line.minor)
          );
        });

  if (line !== null && trackedLatest === undefined) {
    throw new MonitorDataError(`npm returned no stable tracked-line version for ${expectedName}`);
  }

  const selectedRecord =
    trackedLatest === undefined ? latestRecord : metadata.versions[trackedLatest];
  const selectedVersion = trackedLatest ?? publisherLatest;
  if (
    !isRecord(selectedRecord) ||
    selectedRecord.name !== expectedName ||
    selectedRecord.version !== selectedVersion
  ) {
    throw new MonitorDataError(
      `npm returned incomplete selected-version metadata for ${expectedName}`
    );
  }

  return {
    publisherLatest,
    publisherLatestIsStable: parseStableVersion(publisherLatest) !== null,
    globalStable,
    trackedLatest,
    deprecated:
      typeof selectedRecord.deprecated === "string" && selectedRecord.deprecated.trim() !== "",
  };
}

/**
 * @param {object} input
 * @param {string} input.locked
 * @param {ReturnType<typeof inspectNpmMetadata>} input.inspection
 * @param {{outsideLine: "held" | "ignore", reason: string} | null} [input.linePolicy]
 */
export function classifyNpmCurrency({ locked, inspection, linePolicy = null }) {
  const lockedVersion = parseStableVersion(locked);
  if (lockedVersion === null) {
    return {
      latest: inspection.publisherLatest,
      status: /** @type {const} */ ("error"),
      reason: "Locked version is not a stable semantic version.",
    };
  }

  if (inspection.trackedLatest === undefined) {
    return {
      latest: inspection.publisherLatest,
      status: /** @type {const} */ ("review"),
      reason: "The publisher's latest npm tag is a prerelease and needs manual review.",
    };
  }

  const candidate = parseStableVersion(inspection.trackedLatest);
  if (candidate === null) {
    return {
      latest: inspection.trackedLatest,
      status: /** @type {const} */ ("error"),
      reason: "Selected npm version is invalid.",
    };
  }

  if (inspection.deprecated) {
    return {
      latest: candidate.raw,
      status: /** @type {const} */ ("review"),
      reason: "The selected npm release is marked deprecated by its publisher.",
    };
  }

  const comparison = compareStableVersions(lockedVersion, candidate);
  if (comparison < 0) {
    const outsideNote =
      linePolicy?.outsideLine === "held" && inspection.globalStable !== candidate.raw
        ? ` Newer releases outside the tracked line remain held: ${inspection.globalStable}.`
        : "";
    return {
      latest: candidate.raw,
      status: /** @type {const} */ ("update"),
      reason: `A newer stable release is available.${outsideNote}`,
    };
  }
  if (comparison > 0) {
    return {
      latest: candidate.raw,
      status: /** @type {const} */ ("review"),
      reason: "The locked version is newer than the monitored registry channel.",
    };
  }

  if (linePolicy?.outsideLine === "held") {
    const globalVersion = parseStableVersion(inspection.globalStable);
    if (globalVersion !== null && compareStableVersions(globalVersion, candidate) > 0) {
      return {
        latest: globalVersion.raw,
        status: /** @type {const} */ ("held"),
        reason: linePolicy.reason,
      };
    }
  }

  return {
    latest: candidate.raw,
    status: /** @type {const} */ ("current"),
    reason: "Tracked release is current.",
  };
}

/** @param {string} crateName */
export function cratesIndexPath(crateName) {
  if (!CRATE_NAME_PATTERN.test(crateName)) {
    throw new MonitorDataError("Cargo metadata contained an invalid crate name");
  }
  const name = crateName.toLowerCase();
  if (name.length === 1) return `1/${name}`;
  if (name.length === 2) return `2/${name}`;
  if (name.length === 3) return `3/${name[0]}/${name}`;
  return `${name.slice(0, 2)}/${name.slice(2, 4)}/${name}`;
}

/**
 * @param {string} text
 * @param {string} expectedName
 */
export function parseCratesIndex(text, expectedName) {
  if (typeof text !== "string" || !CRATE_NAME_PATTERN.test(expectedName)) {
    throw new MonitorDataError(`crates.io returned invalid metadata for ${expectedName}`);
  }

  /** @type {CrateVersion[]} */
  const versions = [];
  const lines = text.split("\n").filter((line) => line.trim() !== "");
  if (lines.length === 0 || lines.length > 100_000) {
    throw new MonitorDataError(`crates.io returned an invalid version list for ${expectedName}`);
  }

  for (const line of lines) {
    if (line.length > 256_000) {
      throw new MonitorDataError(`crates.io returned an oversized entry for ${expectedName}`);
    }

    /** @type {unknown} */
    let entry;
    try {
      entry = JSON.parse(line);
    } catch {
      throw new MonitorDataError(`crates.io returned malformed JSON for ${expectedName}`);
    }

    if (!isRecord(entry))
      throw new MonitorDataError(`crates.io returned an invalid entry for ${expectedName}`);
    if (entry.v !== undefined && (!Number.isSafeInteger(entry.v) || Number(entry.v) < 1)) {
      throw new MonitorDataError(
        `crates.io returned an invalid schema version for ${expectedName}`
      );
    }
    if (typeof entry.v === "number" && entry.v > 2) continue;
    if (
      typeof entry.name !== "string" ||
      entry.name.toLowerCase() !== expectedName.toLowerCase() ||
      typeof entry.vers !== "string" ||
      parseSemver(entry.vers) === null ||
      typeof entry.yanked !== "boolean"
    ) {
      throw new MonitorDataError(`crates.io returned an invalid entry for ${expectedName}`);
    }
    if (
      entry.rust_version !== undefined &&
      entry.rust_version !== null &&
      parseRustVersion(entry.rust_version) === null
    ) {
      throw new MonitorDataError(`crates.io returned an invalid rust_version for ${expectedName}`);
    }

    versions.push({
      version: entry.vers,
      yanked: entry.yanked,
      rustVersion: typeof entry.rust_version === "string" ? entry.rust_version : null,
    });
  }

  if (versions.length === 0) {
    throw new MonitorDataError(`crates.io returned no supported entries for ${expectedName}`);
  }
  return versions;
}

/**
 * @param {CrateVersion[]} versions
 * @param {(version: ParsedSemver, entry: CrateVersion) => boolean} [predicate]
 */
function maxCrateVersion(versions, predicate = () => true) {
  /** @type {{entry: CrateVersion, parsed: ParsedSemver} | null} */
  let greatest = null;
  for (const entry of versions) {
    if (entry.yanked) continue;
    const parsed = parseStableVersion(entry.version);
    if (parsed === null || !predicate(parsed, entry)) continue;
    if (greatest === null || compareStableVersions(parsed, greatest.parsed) > 0) {
      greatest = { entry, parsed };
    }
  }
  return greatest?.entry;
}

/**
 * @param {object} input
 * @param {string} input.locked
 * @param {CrateVersion[]} input.versions
 * @param {"default" | "manual" | "hold-line"} input.mode
 * @param {string} input.policyReason
 * @param {string} input.msrv
 * @param {string | null} [input.reviewedThrough]
 */
export function classifyCargoCurrency({
  locked,
  versions,
  mode,
  policyReason,
  msrv,
  reviewedThrough = null,
}) {
  const lockedVersion = parseStableVersion(locked);
  const msrvVersion = parseRustVersion(msrv);
  if (lockedVersion === null || msrvVersion === null) {
    return {
      latest: "unknown",
      status: /** @type {const} */ ("error"),
      reason: "Invalid locked version or project MSRV.",
    };
  }

  const lockedEntry = versions.find((entry) => entry.version === locked);
  if (lockedEntry === undefined) {
    return {
      latest: "unknown",
      status: /** @type {const} */ ("error"),
      reason: "The crates.io index response does not contain the locked release.",
    };
  }
  if (lockedEntry.yanked) {
    return {
      latest: locked,
      status: /** @type {const} */ ("review"),
      reason: "The locked crate release is yanked.",
    };
  }

  const globalLatest = maxCrateVersion(versions);
  if (globalLatest === undefined) {
    return {
      latest: "unknown",
      status: /** @type {const} */ ("error"),
      reason: "No non-yanked stable crate release is available.",
    };
  }
  const globalVersion = parseStableVersion(globalLatest.version);
  if (globalVersion === null) {
    return {
      latest: "unknown",
      status: /** @type {const} */ ("error"),
      reason: "Selected crate version is invalid.",
    };
  }

  const inTrackedLine = (/** @type {ParsedSemver} */ version) => {
    return (
      mode !== "hold-line" ||
      (version.major === lockedVersion.major && version.minor === lockedVersion.minor)
    );
  };
  const trackedLatest = maxCrateVersion(versions, inTrackedLine);
  if (trackedLatest === undefined) {
    return {
      latest: globalLatest.version,
      status: /** @type {const} */ ("error"),
      reason: "The tracked Cargo release line has no usable version.",
    };
  }

  if (mode === "manual") {
    const comparison = compareStableVersions(lockedVersion, globalVersion);
    if (comparison > 0) {
      return {
        latest: globalLatest.version,
        status: /** @type {const} */ ("review"),
        reason: "The locked crate is newer than the monitored registry channel.",
      };
    }
    if (comparison === 0) {
      return {
        latest: globalLatest.version,
        status: /** @type {const} */ ("current"),
        reason: "Tracked release is current.",
      };
    }

    const reviewedVersion = reviewedThrough === null ? null : parseStableVersion(reviewedThrough);
    if (reviewedThrough !== null && reviewedVersion === null) {
      return {
        latest: globalLatest.version,
        status: /** @type {const} */ ("error"),
        reason: "The manual-review policy contains an invalid reviewed-through version.",
      };
    }
    if (reviewedVersion !== null && compareStableVersions(reviewedVersion, globalVersion) >= 0) {
      return {
        latest: globalLatest.version,
        status: /** @type {const} */ ("held"),
        reason: `${policyReason} This release has already been reviewed and deliberately retained outside the lock.`,
      };
    }
    return {
      latest: globalLatest.version,
      status: /** @type {const} */ ("review"),
      reason: policyReason,
    };
  }

  const candidate = maxCrateVersion(versions, (version, entry) => {
    if (!inTrackedLine(version)) return false;
    if (entry.rustVersion === null) return true;
    const declaredRust = parseRustVersion(entry.rustVersion);
    return declaredRust !== null && compareRustVersions(declaredRust, msrvVersion) <= 0;
  });
  if (candidate === undefined) {
    return {
      latest: trackedLatest.version,
      status: /** @type {const} */ ("error"),
      reason: `No non-yanked stable release is usable at the project's declared Rust ${msrv} floor.`,
    };
  }

  const candidateVersion = parseStableVersion(candidate.version);
  const trackedVersion = parseStableVersion(trackedLatest.version);
  if (candidateVersion === null || trackedVersion === null) {
    return {
      latest: "unknown",
      status: /** @type {const} */ ("error"),
      reason: "Selected crate version is invalid.",
    };
  }

  const comparison = compareStableVersions(lockedVersion, candidateVersion);
  if (comparison > 0) {
    return {
      latest: candidate.version,
      status: /** @type {const} */ ("review"),
      reason: "The locked crate is newer than the monitored registry channel.",
    };
  }

  const blockedByRust = compareStableVersions(trackedVersion, candidateVersion) > 0;
  const outsideTrackedLine =
    mode === "hold-line" && compareStableVersions(globalVersion, trackedVersion) > 0;
  if (comparison < 0) {
    const rustNote =
      candidate.rustVersion === null
        ? `The crate does not declare rust_version; exact Rust ${msrv} verification is required.`
        : `The crate declares Rust ${candidate.rustVersion}; the resolved graph still requires exact Rust ${msrv} verification.`;
    const blockedNote = blockedByRust
      ? ` Newer tracked release ${trackedLatest.version} declares a Rust version above ${msrv} and remains held.`
      : "";
    const outsideNote = outsideTrackedLine
      ? ` ${policyReason} Latest upstream: ${globalLatest.version}.`
      : "";
    return {
      latest: candidate.version,
      status: /** @type {const} */ ("update"),
      reason: `${rustNote}${blockedNote}${outsideNote}`,
    };
  }

  if (blockedByRust) {
    return {
      latest: trackedLatest.version,
      status: /** @type {const} */ ("held"),
      reason: `The newer tracked release declares Rust ${trackedLatest.rustVersion}, above the project's Rust ${msrv} floor.`,
    };
  }

  if (outsideTrackedLine) {
    return {
      latest: globalLatest.version,
      status: /** @type {const} */ ("held"),
      reason: policyReason,
    };
  }

  return {
    latest: candidate.version,
    status: /** @type {const} */ ("current"),
    reason: "Tracked release is current.",
  };
}

/** @param {unknown} metadata */
export function collectCargoTargets(metadata) {
  if (
    !isRecord(metadata) ||
    metadata.version !== 1 ||
    !Array.isArray(metadata.packages) ||
    !Array.isArray(metadata.workspace_members) ||
    !isRecord(metadata.resolve) ||
    !Array.isArray(metadata.resolve.nodes)
  ) {
    throw new MonitorDataError("cargo metadata returned an invalid format-version 1 document");
  }

  const packageById = new Map();
  for (const packageEntry of metadata.packages) {
    if (!isRecord(packageEntry) || typeof packageEntry.id !== "string") {
      throw new MonitorDataError("cargo metadata returned an invalid package entry");
    }
    packageById.set(packageEntry.id, packageEntry);
  }

  const nodeById = new Map();
  for (const node of metadata.resolve.nodes) {
    if (!isRecord(node) || typeof node.id !== "string" || !Array.isArray(node.deps)) {
      throw new MonitorDataError("cargo metadata returned an invalid resolve node");
    }
    nodeById.set(node.id, node);
  }

  /** @type {Map<string, CargoTarget>} */
  const targets = new Map();
  /** @type {{raw: string, parsed: {major: number, minor: number, patch: number}} | null} */
  let projectMsrv = null;
  for (const workspaceId of metadata.workspace_members) {
    if (typeof workspaceId !== "string")
      throw new MonitorDataError("cargo metadata returned an invalid workspace member");
    const workspacePackage = packageById.get(workspaceId);
    const workspaceNode = nodeById.get(workspaceId);
    if (
      !isRecord(workspacePackage) ||
      !Array.isArray(workspacePackage.dependencies) ||
      !isRecord(workspaceNode) ||
      !Array.isArray(workspaceNode.deps)
    ) {
      throw new MonitorDataError("cargo metadata is missing a workspace package or resolve node");
    }

    const workspaceMsrv = parseRustVersion(workspacePackage.rust_version);
    if (typeof workspacePackage.rust_version !== "string" || workspaceMsrv === null) {
      throw new MonitorDataError("Every Cargo workspace package must declare a valid rust_version");
    }
    if (projectMsrv === null || compareRustVersions(workspaceMsrv, projectMsrv.parsed) > 0) {
      projectMsrv = { raw: workspacePackage.rust_version, parsed: workspaceMsrv };
    }

    const declaredRegistryNames = new Set();
    for (const dependency of workspacePackage.dependencies) {
      if (!isRecord(dependency))
        throw new MonitorDataError("cargo metadata returned an invalid dependency declaration");
      if (
        typeof dependency.source === "string" &&
        (dependency.source.startsWith("registry+") || dependency.source.startsWith("sparse+"))
      ) {
        if (!CRATES_IO_SOURCE_IDS.has(dependency.source)) {
          throw new MonitorDataError(
            `Non-crates.io registry dependency ${String(dependency.name)} is not supported`
          );
        }
        if (dependency.optional === true) {
          throw new MonitorDataError(
            `Optional Cargo dependency ${String(dependency.name)} has no guaranteed locked edge`
          );
        }
        if (typeof dependency.name !== "string")
          throw new MonitorDataError("cargo metadata returned a dependency without a name");
        declaredRegistryNames.add(dependency.name);
      }
    }

    const resolvedRegistryNames = new Set();
    for (const dependencyEdge of workspaceNode.deps) {
      if (!isRecord(dependencyEdge) || typeof dependencyEdge.pkg !== "string") {
        throw new MonitorDataError("cargo metadata returned an invalid direct dependency edge");
      }
      const dependencyPackage = packageById.get(dependencyEdge.pkg);
      if (!isRecord(dependencyPackage) || typeof dependencyPackage.source !== "string") continue;
      if (
        !dependencyPackage.source.startsWith("registry+") &&
        !dependencyPackage.source.startsWith("sparse+")
      ) {
        continue;
      }
      if (!CRATES_IO_SOURCE_IDS.has(dependencyPackage.source)) {
        throw new MonitorDataError(
          `Non-crates.io registry dependency ${String(dependencyPackage.name)} is not supported`
        );
      }
      if (
        typeof dependencyPackage.name !== "string" ||
        !CRATE_NAME_PATTERN.test(dependencyPackage.name) ||
        parseStableVersion(dependencyPackage.version) === null
      ) {
        throw new MonitorDataError("cargo metadata returned an invalid locked direct crate");
      }

      resolvedRegistryNames.add(dependencyPackage.name);
      const kinds = Array.isArray(dependencyEdge.dep_kinds)
        ? dependencyEdge.dep_kinds.map((kind) => {
            return isRecord(kind) && typeof kind.kind === "string" ? kind.kind : "normal";
          })
        : ["normal"];
      const existing = targets.get(dependencyPackage.name);
      if (existing !== undefined && existing.locked !== dependencyPackage.version) {
        throw new MonitorDataError(
          `Direct crate ${dependencyPackage.name} resolves to multiple versions`
        );
      }
      targets.set(dependencyPackage.name, {
        name: dependencyPackage.name,
        locked: String(dependencyPackage.version),
        kinds: [...new Set([...(existing?.kinds ?? []), ...kinds])].sort(),
      });
    }

    for (const declaredName of declaredRegistryNames) {
      if (!resolvedRegistryNames.has(declaredName)) {
        throw new MonitorDataError(`Cargo dependency ${declaredName} has no locked direct edge`);
      }
    }
  }

  if (projectMsrv === null) {
    throw new MonitorDataError("cargo metadata returned no workspace Rust version");
  }
  return {
    targets: [...targets.values()].sort((left, right) => left.name.localeCompare(right.name)),
    msrv: projectMsrv.raw,
  };
}

/**
 * @param {{path: string, content: string}[]} files
 */
export function parseActionReferences(files) {
  /** @type {Map<string, ActionTarget>} */
  const actions = new Map();
  /** @type {{dependency: string, reason: string}[]} */
  const errors = [];

  for (const file of files) {
    if (typeof file.path !== "string" || typeof file.content !== "string") {
      throw new MonitorDataError("Workflow source has an invalid shape");
    }
    const lines = file.content.split(/\r?\n/u);
    /** @type {number | null} */
    let blockScalarParentIndent = null;
    for (let index = 0; index < lines.length; index += 1) {
      const line = lines[index];
      const lineIndent = /^\s*/u.exec(line)?.[0].length ?? 0;
      if (blockScalarParentIndent !== null) {
        if (line.trim() === "" || lineIndent > blockScalarParentIndent) continue;
        blockScalarParentIndent = null;
      }
      if (/^\s*(?:-\s*)?[A-Za-z0-9_-]+:\s*[|>][+-]?\s*(?:#.*)?$/u.test(line)) {
        blockScalarParentIndent = lineIndent;
        continue;
      }
      const match =
        /^\s*(?:-\s*)?uses:\s*(?:"([^"]+)"|'([^']+)'|([^\s#]+))\s*(?:#\s*(.+?))?\s*$/u.exec(line);
      if (match === null) {
        const unquotedKey = (() => {
          let singleQuoted = false;
          let doubleQuoted = false;
          for (let position = 0; position < line.length; position += 1) {
            const character = line[position];
            if (doubleQuoted && character === "\\") {
              position += 1;
              continue;
            }
            if (!doubleQuoted && character === "'") {
              if (singleQuoted && line[position + 1] === "'") {
                position += 1;
                continue;
              }
              singleQuoted = !singleQuoted;
              continue;
            }
            if (!singleQuoted && character === '"') {
              doubleQuoted = !doubleQuoted;
              continue;
            }
            if (singleQuoted || doubleQuoted) continue;
            if (character === "#") break;
            if (line.slice(position, position + 4) !== "uses") continue;
            const before = line.slice(0, position).trim();
            const after = line.slice(position + 4);
            if (!/^\s*:/u.test(after)) continue;
            if (before === "" || before === "-" || before.endsWith("{") || before.endsWith(",")) {
              return true;
            }
          }
          return false;
        })();
        const quotedKey = /^\s*(?:-\s*)?(?:"uses"|'uses')\s*:/u.test(line);
        if (unquotedKey || quotedKey) {
          const location = `${file.path}:${index + 1}`;
          errors.push({
            dependency: "unsupported uses syntax",
            reason: `${location} contains a uses key that the monitor cannot parse safely.`,
          });
        }
        continue;
      }
      const reference = match[1] ?? match[2] ?? match[3];
      const annotation = match[4]?.trim();
      if (reference.startsWith("./") || reference.startsWith("docker://")) continue;

      const location = `${file.path}:${index + 1}`;
      const atIndex = reference.lastIndexOf("@");
      if (atIndex <= 0) {
        errors.push({
          dependency: reference.slice(0, MAX_REPORT_FIELD_LENGTH),
          reason: `${location} has no action ref.`,
        });
        continue;
      }
      const actionPath = reference.slice(0, atIndex);
      const pin = reference.slice(atIndex + 1);
      const pathParts = actionPath.split("/");
      if (
        pathParts.length < 2 ||
        !ACTION_COMPONENT_PATTERN.test(pathParts[0]) ||
        !ACTION_COMPONENT_PATTERN.test(pathParts[1])
      ) {
        errors.push({
          dependency: actionPath.slice(0, MAX_REPORT_FIELD_LENGTH),
          reason: `${location} has an invalid action repository.`,
        });
        continue;
      }
      const repository = `${pathParts[0]}/${pathParts[1]}`;
      if (!FULL_COMMIT_SHA_PATTERN.test(pin)) {
        errors.push({
          dependency: repository,
          reason: `${location} is not pinned to a full immutable commit SHA.`,
        });
        continue;
      }
      const tagVersion = annotation?.startsWith("v")
        ? parseStableVersion(annotation.slice(1))
        : null;
      if (tagVersion === null) {
        errors.push({
          dependency: repository,
          reason: `${location} needs an exact # vMAJOR.MINOR.PATCH annotation.`,
        });
        continue;
      }

      const existing = actions.get(repository);
      if (
        existing !== undefined &&
        (existing.pin.toLowerCase() !== pin.toLowerCase() || existing.declaredTag !== annotation)
      ) {
        errors.push({
          dependency: repository,
          reason: "The same action repository is pinned inconsistently across workflow files.",
        });
        continue;
      }
      actions.set(repository, {
        repository,
        pin: pin.toLowerCase(),
        declaredTag: annotation,
        locations: [...(existing?.locations ?? []), location],
      });
    }
  }

  return {
    actions: [...actions.values()].sort((left, right) =>
      left.repository.localeCompare(right.repository)
    ),
    errors,
  };
}

/** @param {unknown} releases */
export function inspectGitHubReleases(releases) {
  if (!Array.isArray(releases) || releases.length === 0 || releases.length > 300) {
    throw new MonitorDataError("GitHub returned an invalid action release list");
  }

  /** @type {{tag: string, version: ParsedSemver} | null} */
  let greatest = null;
  for (const release of releases) {
    if (
      !isRecord(release) ||
      typeof release.draft !== "boolean" ||
      typeof release.prerelease !== "boolean" ||
      typeof release.tag_name !== "string" ||
      release.tag_name.length > MAX_VERSION_LENGTH
    ) {
      throw new MonitorDataError("GitHub returned an invalid action release entry");
    }
    if (release.draft || release.prerelease || !release.tag_name.startsWith("v")) continue;
    const version = parseStableVersion(release.tag_name.slice(1));
    if (version === null) continue;
    if (greatest === null || compareStableVersions(version, greatest.version) > 0) {
      greatest = { tag: release.tag_name, version };
    }
  }

  if (greatest === null) {
    throw new MonitorDataError("GitHub returned no stable vMAJOR.MINOR.PATCH action release");
  }
  return greatest;
}

/** @param {string} value */
export function inspectResolvedCommitSha(value) {
  const sha = value.trim().toLowerCase();
  if (!FULL_COMMIT_SHA_PATTERN.test(sha)) {
    throw new MonitorDataError("GitHub did not resolve the action tag to a full commit SHA");
  }
  return sha;
}

/**
 * @param {object} input
 * @param {ActionTarget} input.action
 * @param {{tag: string, version: ParsedSemver}} input.latestRelease
 * @param {string} input.declaredSha
 * @param {string} input.latestSha
 */
export function classifyActionCurrency({ action, latestRelease, declaredSha, latestSha }) {
  if (action.pin !== declaredSha) {
    return {
      latest: latestRelease.tag,
      status: /** @type {const} */ ("error"),
      reason: "The version annotation does not resolve to the pinned commit.",
    };
  }
  const declaredVersion = parseStableVersion(action.declaredTag.slice(1));
  if (declaredVersion === null) {
    return {
      latest: latestRelease.tag,
      status: /** @type {const} */ ("error"),
      reason: "The declared action version is invalid.",
    };
  }
  const comparison = compareStableVersions(declaredVersion, latestRelease.version);
  if (comparison < 0) {
    return {
      latest: latestRelease.tag,
      status: /** @type {const} */ ("update"),
      reason: "A newer stable action release is available.",
    };
  }
  if (comparison > 0) {
    return {
      latest: latestRelease.tag,
      status: /** @type {const} */ ("review"),
      reason: "The pinned action version is newer than the publisher's latest release channel.",
    };
  }
  if (action.pin !== latestSha) {
    return {
      latest: latestRelease.tag,
      status: /** @type {const} */ ("review"),
      reason: "The upstream release tag moved to a different commit.",
    };
  }
  return {
    latest: latestRelease.tag,
    status: /** @type {const} */ ("current"),
    reason: "Pinned release is current and its tag resolves to the pinned commit.",
  };
}

/** @param {unknown} value */
export function sanitizeReportField(value) {
  const flattened = String(value)
    .replace(/[\r\n]+/gu, " ")
    .replace(/\\/gu, "\\\\")
    .replace(/\|/gu, "\\|")
    .replace(/`/gu, "\\`")
    .replace(/\[/gu, "\\[")
    .replace(/\]/gu, "\\]")
    .replace(/</gu, "&lt;")
    .replace(/>/gu, "&gt;")
    .trim();
  return flattened.length <= MAX_REPORT_FIELD_LENGTH
    ? flattened
    : `${flattened.slice(0, MAX_REPORT_FIELD_LENGTH - 1)}…`;
}

/** @param {CurrencyRow[]} rows */
export function dependencyWatchExitCode(rows) {
  if (rows.some((row) => row.status === "error")) return 2;
  if (rows.some((row) => row.status === "update" || row.status === "review")) return 1;
  return 0;
}

/** @param {CurrencyRow[]} rows */
export function renderDependencyReport(rows) {
  if (rows.length > MAX_REPORT_ROWS) {
    throw new MonitorDataError(`Dependency report exceeds the ${MAX_REPORT_ROWS}-row safety cap`);
  }

  const statusLabels = {
    current: "current",
    update: "update available",
    review: "manual review",
    held: "held by policy",
    error: "monitor error",
  };
  const statusOrder = { error: 0, review: 1, update: 2, held: 3, current: 4 };
  const sortedRows = [...rows].sort((left, right) => {
    return (
      statusOrder[left.status] - statusOrder[right.status] ||
      left.ecosystem.localeCompare(right.ecosystem) ||
      left.dependency.localeCompare(right.dependency)
    );
  });
  const counts = Object.fromEntries(
    Object.keys(statusLabels).map((status) => [
      status,
      sortedRows.filter((row) => row.status === status).length,
    ])
  );

  const lines = [
    "# Dependency Watch",
    "",
    "This read-only check monitors ordinary direct dependency updates. A red workflow run means maintenance is available; it does not mean the product build is broken.",
    "",
    "Security vulnerabilities remain a separate GitHub Dependabot Alerts responsibility. This workflow cannot push, create branches, or open pull requests.",
    "",
    `Current: ${counts.current} · Updates: ${counts.update} · Manual review: ${counts.review} · Held: ${counts.held} · Monitor errors: ${counts.error}`,
    "",
    "| Ecosystem | Dependency | Locked | Latest observed | Status | Notes |",
    "| --- | --- | --- | --- | --- | --- |",
    ...sortedRows.map((row) => {
      return `| ${sanitizeReportField(row.ecosystem)} | ${sanitizeReportField(row.dependency)} | ${sanitizeReportField(row.locked)} | ${sanitizeReportField(row.latest)} | ${statusLabels[row.status]} | ${sanitizeReportField(row.reason)} |`;
    }),
    "",
  ];
  const report = lines.join("\n");
  if (Buffer.byteLength(report, "utf8") > MAX_SUMMARY_BYTES) {
    throw new MonitorDataError("Dependency report exceeds the GitHub job-summary safety cap");
  }
  return report;
}

/**
 * @param {string | null} value
 * @param {number} [now]
 */
export function retryAfterMilliseconds(value, now = Date.now()) {
  if (value === null) return 0;
  const seconds = Number(value);
  if (Number.isFinite(seconds) && seconds >= 0) return Math.min(seconds * 1_000, 60_000);
  const date = Date.parse(value);
  if (Number.isNaN(date)) return 0;
  return Math.min(Math.max(date - now, 0), 60_000);
}

/**
 * @param {Response} response
 * @param {number} maxBytes
 */
export async function readBoundedResponse(response, maxBytes) {
  const declaredLength = Number(response.headers.get("content-length"));
  if (Number.isFinite(declaredLength) && declaredLength > maxBytes) {
    throw new MonitorDataError("Remote metadata exceeds the response-size safety cap");
  }
  if (response.body === null) return "";

  const reader = response.body.getReader();
  const decoder = new TextDecoder("utf-8", { fatal: true });
  let total = 0;
  let text = "";
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > maxBytes) {
      await reader.cancel();
      throw new MonitorDataError("Remote metadata exceeds the response-size safety cap");
    }
    text += decoder.decode(value, { stream: true });
  }
  text += decoder.decode();
  return text;
}

/**
 * @param {string} url
 * @param {object} options
 * @param {Record<string, string>} [options.headers]
 * @param {number} [options.maxBytes]
 * @param {number} [options.attempts]
 * @param {typeof fetch} [options.fetchImpl]
 * @param {(milliseconds: number) => Promise<void>} [options.sleep]
 */
export async function fetchTextWithPolicy(
  url,
  {
    headers = {},
    maxBytes = 32 * 1024 * 1024,
    attempts = 2,
    fetchImpl = fetch,
    sleep = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds)),
  }
) {
  /** @type {unknown} */
  let lastError;
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    try {
      const response = await fetchImpl(url, {
        headers,
        redirect: "manual",
        signal: AbortSignal.timeout(15_000),
      });
      if (response.ok) return await readBoundedResponse(response, maxBytes);

      const retryAfter = response.headers.get("retry-after");
      const rateLimitReset = Number(response.headers.get("x-ratelimit-reset"));
      const rateLimitedForbidden =
        response.status === 403 &&
        (retryAfter !== null || response.headers.get("x-ratelimit-remaining") === "0");
      const retryable = response.status === 429 || rateLimitedForbidden || response.status >= 500;
      if (!retryable || attempt === attempts) {
        throw new MonitorDataError(`Remote metadata request returned HTTP ${response.status}`);
      }
      const requestedDelay = retryAfterMilliseconds(retryAfter);
      const resetDelay = Number.isFinite(rateLimitReset)
        ? Math.min(Math.max(rateLimitReset * 1_000 - Date.now(), 0), 60_000)
        : 0;
      const fallbackDelay = rateLimitedForbidden ? 60_000 : attempt * 500;
      await sleep(Math.max(requestedDelay, resetDelay, fallbackDelay));
    } catch (error) {
      lastError = error;
      if (error instanceof MonitorDataError || attempt === attempts) break;
      await sleep(attempt * 500);
    }
  }

  if (lastError instanceof MonitorDataError) throw lastError;
  throw new MonitorDataError("Remote metadata request failed or timed out");
}

/**
 * @template T, R
 * @param {T[]} values
 * @param {number} limit
 * @param {(value: T, index: number) => Promise<R>} worker
 */
export async function mapWithConcurrency(values, limit, worker) {
  if (!Number.isInteger(limit) || limit < 1)
    throw new MonitorDataError("Concurrency limit must be positive");
  /** @type {R[]} */
  const results = new Array(values.length);
  let nextIndex = 0;

  async function runWorker() {
    while (nextIndex < values.length) {
      const index = nextIndex;
      nextIndex += 1;
      results[index] = await worker(values[index], index);
    }
  }

  await Promise.all(Array.from({ length: Math.min(limit, values.length) }, () => runWorker()));
  return results;
}

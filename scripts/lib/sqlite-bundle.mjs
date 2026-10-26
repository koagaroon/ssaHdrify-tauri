import { createHash } from "node:crypto";
import { lstatSync, readFileSync } from "node:fs";
import { dirname, isAbsolute, join } from "node:path";

export const sqliteBundlePath = "src-tauri/sqlite-bundle.json";
const registrySource = "registry+https://github.com/rust-lang/crates.io-index";
const digestPattern = /^[a-f0-9]{64}$/u;
const stableVersionPattern = /^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)$/u;
const reviewedFiles = [
  "Cargo.toml",
  "build.rs",
  "LICENSE",
  "sqlite3/sqlite3.c",
  "sqlite3/sqlite3.h",
  "sqlite3/sqlite3ext.h",
  "sqlite3/bindgen_bundled_version.rs",
  "sqlite3/bindgen_bundled_version_ext.rs",
];
const alternateBuildFeatures = [
  "buildtime_bindgen",
  "loadable_extension",
  "sqlcipher",
  "bundled-sqlcipher",
  "bundled-sqlcipher-vendored-openssl",
];

/** @typedef {{version: string, sourceId: string, url: string, sha3_256: string, sourceSha3_256: string}} SqliteIdentity */
/** @typedef {{schemaVersion: number, crate: {name: string, version: string, source: string, url: string, sha256: string, features: string[]}, sqlite: SqliteIdentity, files: Record<string, string>}} SqliteBundle */
/** @typedef {{id: string, name: string, version: string, source: string, manifest_path: string}} SqlitePackage */

/** @param {unknown} value @returns {value is Record<string, unknown>} */
const isRecord = (value) => typeof value === "object" && value !== null && !Array.isArray(value);

/** @param {unknown} value @returns {SqliteBundle} */
export function inspectSqliteBundle(value) {
  if (!isRecord(value) || value.schemaVersion !== 1 || !isRecord(value.crate)) {
    throw new Error("SQLite bundle provenance has an invalid schema.");
  }
  const crate = value.crate;
  const features = crate.features;
  if (
    crate.name !== "libsqlite3-sys" ||
    typeof crate.version !== "string" ||
    !stableVersionPattern.test(crate.version) ||
    crate.source !== registrySource ||
    crate.url !==
      `https://static.crates.io/crates/libsqlite3-sys/libsqlite3-sys-${crate.version}.crate` ||
    typeof crate.sha256 !== "string" ||
    !digestPattern.test(crate.sha256) ||
    !Array.isArray(features) ||
    !features.every(
      (feature) => typeof feature === "string" && /^[a-zA-Z0-9_-]+$/u.test(feature)
    ) ||
    new Set(features).size !== features.length ||
    !["bundled", "bundled_bindings", "cc"].every((feature) => features.includes(feature)) ||
    alternateBuildFeatures.some((feature) => features.includes(feature))
  ) {
    throw new Error("SQLite bundle provenance has an invalid published crate identity.");
  }
  const sqlite = value.sqlite;
  const version =
    isRecord(sqlite) && typeof sqlite.version === "string"
      ? /^3\.(\d{1,2})\.(\d{1,2})(?:\.(\d{1,2}))?$/u.exec(sqlite.version)
      : null;
  const encoded =
    version &&
    `3${version[1].padStart(2, "0")}${version[2].padStart(2, "0")}${(version[3] ?? "0").padStart(2, "0")}`;
  if (
    !isRecord(sqlite) ||
    !version ||
    typeof sqlite.url !== "string" ||
    !new RegExp(`^https://sqlite\\.org/20\\d{2}/sqlite-amalgamation-${encoded}\\.zip$`, "u").test(
      sqlite.url
    ) ||
    typeof sqlite.sourceId !== "string" ||
    !/^20\d{2}-\d{2}-\d{2} \d{2}:\d{2}:\d{2} [a-f0-9]{64}$/u.test(sqlite.sourceId) ||
    ![sqlite.sha3_256, sqlite.sourceSha3_256].every(
      (digest) => typeof digest === "string" && digestPattern.test(digest)
    )
  ) {
    throw new Error("SQLite bundle provenance has an invalid SQLite source identity.");
  }
  if (
    !isRecord(value.files) ||
    JSON.stringify(Object.keys(value.files).sort()) !== JSON.stringify([...reviewedFiles].sort()) ||
    !Object.values(value.files).every(
      (digest) => typeof digest === "string" && digestPattern.test(digest)
    )
  ) {
    throw new Error(
      "SQLite bundle provenance must identify every reviewed source and binding file."
    );
  }
  return /** @type {SqliteBundle} */ (/** @type {unknown} */ (value));
}

/** @param {string} block @param {string} key */
function lockField(block, key) {
  const fields = [...block.matchAll(new RegExp(`^${key}\\s*=\\s*"([^"\\r\\n]+)"\\s*$`, "gmu"))];
  if (fields.length !== 1) throw new Error(`SQLite Cargo lock entry has an invalid ${key}.`);
  return fields[0][1];
}

/** @param {unknown} metadata @param {unknown} provenance @param {unknown} lockText */
export function selectSqliteBundle(metadata, provenance, lockText) {
  const bundle = inspectSqliteBundle(provenance);
  if (
    !isRecord(metadata) ||
    !Array.isArray(metadata.packages) ||
    !isRecord(metadata.resolve) ||
    !Array.isArray(metadata.resolve.nodes)
  )
    throw new Error("Cargo metadata is missing the SQLite dependency graph.");
  const packages = metadata.packages.filter(
    (pkg) => isRecord(pkg) && pkg.name === "libsqlite3-sys"
  );
  const pkg = packages[0];
  if (
    packages.length !== 1 ||
    !isRecord(pkg) ||
    typeof pkg.id !== "string" ||
    pkg.version !== bundle.crate.version ||
    pkg.source !== bundle.crate.source ||
    typeof pkg.manifest_path !== "string" ||
    !isAbsolute(pkg.manifest_path)
  )
    throw new Error("Expected exactly one reviewed crates.io libsqlite3-sys package.");
  const nodes = metadata.resolve.nodes.filter((node) => isRecord(node) && node.id === pkg.id);
  const node = nodes[0];
  const resolvedFeatures = isRecord(node) ? node.features : null;
  if (
    nodes.length !== 1 ||
    !isRecord(node) ||
    !Array.isArray(resolvedFeatures) ||
    !resolvedFeatures.every((feature) => typeof feature === "string") ||
    !["bundled", "bundled_bindings", "cc"].every((feature) => resolvedFeatures.includes(feature)) ||
    JSON.stringify([...resolvedFeatures].sort()) !==
      JSON.stringify([...bundle.crate.features].sort())
  )
    throw new Error(
      "The reviewed SQLite package must use the ordinary bundled build and pregenerated bindings."
    );
  if (typeof metadata.resolve.root !== "string")
    throw new Error("The SQLite dependency graph has no root package.");
  const graph = new Map();
  for (const candidate of metadata.resolve.nodes) {
    if (
      !isRecord(candidate) ||
      typeof candidate.id !== "string" ||
      graph.has(candidate.id) ||
      !Array.isArray(candidate.deps)
    ) {
      throw new Error("The SQLite dependency graph has missing or ambiguous nodes.");
    }
    graph.set(candidate.id, candidate);
  }
  const visited = new Set();
  const queue = [metadata.resolve.root];
  for (let index = 0; index < queue.length; index++) {
    const id = queue[index];
    if (visited.has(id)) continue;
    visited.add(id);
    const candidate = graph.get(id);
    if (!candidate) throw new Error("The SQLite dependency graph has an unresolved edge.");
    for (const edge of candidate.deps) {
      if (
        !isRecord(edge) ||
        typeof edge.pkg !== "string" ||
        !Array.isArray(edge.dep_kinds) ||
        edge.dep_kinds.length === 0 ||
        edge.dep_kinds.some(
          (kind) =>
            !isRecord(kind) || (kind.kind !== null && kind.kind !== "build" && kind.kind !== "dev")
        )
      ) {
        throw new Error("The SQLite dependency graph has an invalid dependency edge.");
      }
      if (edge.dep_kinds.some((kind) => kind.kind !== "dev")) queue.push(edge.pkg);
    }
  }
  if (!visited.has(pkg.id))
    throw new Error("The reviewed SQLite package is absent from the application dependency graph.");
  if (
    typeof lockText !== "string" ||
    !/^version\s*=\s*4\s*$/mu.test(lockText.split("[[package]]")[0])
  ) {
    throw new Error("SQLite bundle verification requires a Cargo version 4 lockfile.");
  }
  const locks = lockText
    .split(/^\[\[package\]\]\s*$/mu)
    .slice(1)
    .map((block) => block.split(/^\[/mu)[0])
    .filter((block) => /^name\s*=\s*"libsqlite3-sys"\s*$/mu.test(block));
  if (locks.length !== 1) throw new Error("Expected exactly one SQLite Cargo lock entry.");
  const locked = locks[0];
  if (
    lockField(locked, "name") !== bundle.crate.name ||
    lockField(locked, "version") !== bundle.crate.version ||
    lockField(locked, "source") !== bundle.crate.source ||
    lockField(locked, "checksum") !== bundle.crate.sha256
  )
    throw new Error("The SQLite Cargo lock identity differs from the reviewed published bundle.");
  return { package: /** @type {SqlitePackage} */ (/** @type {unknown} */ (pkg)), bundle };
}

/** @param {string} path */
function readBoundedSource(path) {
  const stat = lstatSync(path);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 16 * 1024 * 1024) {
    throw new Error("SQLite bundle source must be a bounded regular file.");
  }
  return readFileSync(path);
}

/**
 * Verify Cargo's selected source against the reviewed published bundle.
 * @param {{metadata: unknown, lockText: unknown, provenance: unknown, readSource?: (path: string) => Buffer}} input
 */
export function verifySqliteBundle({
  metadata,
  lockText,
  provenance,
  readSource = readBoundedSource,
}) {
  const { package: pkg, bundle } = selectSqliteBundle(metadata, provenance, lockText);
  const directory = dirname(pkg.manifest_path);
  const contents = new Map(
    reviewedFiles.map((file) => {
      const bytes = readSource(join(directory, file));
      if (
        !Buffer.isBuffer(bytes) ||
        bytes.length > 16 * 1024 * 1024 ||
        createHash("sha256").update(bytes).digest("hex") !== bundle.files[file]
      ) {
        throw new Error(`SQLite bundle source differs from the reviewed package: ${file}`);
      }
      return [file, bytes];
    })
  );
  /** @param {string} file */
  const content = (file) => {
    const bytes = contents.get(file);
    if (!bytes) throw new Error(`Reviewed SQLite source is missing: ${file}`);
    return bytes;
  };
  if (
    createHash("sha3-256").update(content("sqlite3/sqlite3.c")).digest("hex") !==
    bundle.sqlite.sourceSha3_256
  ) {
    throw new Error("SQLite amalgamation does not match the reviewed official source checksum.");
  }
  const header = content("sqlite3/sqlite3.h").toString("utf8");
  if (
    /^#define SQLITE_VERSION\s+"([^"]+)"$/mu.exec(header)?.[1] !== bundle.sqlite.version ||
    /^#define SQLITE_SOURCE_ID\s+"([^"]+)"$/mu.exec(header)?.[1] !== bundle.sqlite.sourceId
  )
    throw new Error("SQLite header identity differs from the reviewed source.");
  for (const file of ["bindgen_bundled_version.rs", "bindgen_bundled_version_ext.rs"]) {
    const bindings = content(`sqlite3/${file}`).toString("utf8");
    for (const name of [
      "SQLITE_VERSION",
      "SQLITE_SOURCE_ID",
      "SQLITE_SCM_BRANCH",
      "SQLITE_SCM_TAGS",
      "SQLITE_SCM_DATETIME",
    ]) {
      const expected = new RegExp(`^#define ${name}\\s+"([^"]+)"$`, "mu").exec(header)?.[1];
      const actual = new RegExp(`pub const ${name}: [^=]+=\\s*c"([^"]+)";`, "u").exec(
        bindings
      )?.[1];
      if (!expected || actual !== expected)
        throw new Error(`SQLite binding metadata differs from the header: ${file}: ${name}`);
    }
    const number = /^#define SQLITE_VERSION_NUMBER\s+(\d+)$/mu.exec(header)?.[1];
    if (
      !number ||
      /pub const SQLITE_VERSION_NUMBER: i32 = (\d+);/u.exec(bindings)?.[1] !== number
    ) {
      throw new Error(`SQLite binding version number differs from the header: ${file}`);
    }
  }
  return { package: pkg, sqlite: bundle.sqlite };
}

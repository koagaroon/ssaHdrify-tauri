import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

export const nativeTarget = "x86_64-pc-windows-msvc";
export const nativeManifestPath = "src/assets/licenses/native-notices.json";
export const nativeTextPath = "src/assets/licenses/native-notices.txt";
export const nativeSourcesPath = "scripts/native-notice-sources.json";
export const noticeSeparator = "\n\n" + "=".repeat(72) + "\n\n";

/** @param {string} text */
export const normalizeText = (text) => text.replace(/\r\n/gu, "\n");
/** @param {string} text */
export const textDigest = (text) => createHash("sha256").update(normalizeText(text)).digest("hex");

/** @param {string} name */
export function isNativeLicenseFile(name) {
  // A source file such as V8's copying-phase.cc is not a license document.
  return (
    /^(?:licen[cs]e|copying|copyright|notice|unlicense)(?:$|[._-])/iu.test(name) &&
    !/\.(?:[cm]?[jt]sx?|rs|c|cc|cpp|cxx|h|hh|hpp|hxx|s|py|go|sh|ps1|cmake|gn|gni)$/iu.test(name)
  );
}

/** @param {string} root */
export function nativeInputs(root) {
  const required = [
    "src-tauri/Cargo.lock",
    "src-tauri/Cargo.toml",
    "rust-toolchain.toml",
    "src-tauri/vendor/libsqlite3-sys/Cargo.toml",
    "src-tauri/vendor/libsqlite3-sys/LICENSE",
    nativeSourcesPath,
  ];
  const optional = [
    ".cargo/config",
    ".cargo/config.toml",
    "src-tauri/.cargo/config",
    "src-tauri/.cargo/config.toml",
  ];
  return Object.fromEntries(
    [...required, ...optional].map((path) => [
      path,
      optional.includes(path) && !existsSync(join(root, path))
        ? null
        : textDigest(readFileSync(join(root, path), "utf8")),
    ])
  );
}

/**
 * Frontend builds use the reviewed snapshot without requiring Cargo or network access.
 * Native CI separately reproduces it from the locked dependency sources.
 * @param {string} root
 */
export function buildNativeNotices(root) {
  const manifest = JSON.parse(readFileSync(join(root, nativeManifestPath), "utf8"));
  const text = normalizeText(readFileSync(join(root, nativeTextPath), "utf8"));
  if (
    manifest?.schemaVersion !== 1 ||
    manifest.target !== nativeTarget ||
    JSON.stringify(manifest.inputs) !== JSON.stringify(nativeInputs(root)) ||
    manifest.sha256 !== textDigest(text) ||
    !text.trim()
  ) {
    throw new Error(
      "Native notices are missing, stale, or changed. Review native license sources, then run node scripts/native-notices.mjs --write."
    );
  }
  return text;
}

/**
 * @typedef {{id: string, name: string, version: string, license: string | null, license_file: string | null, manifest_path: string, source: string | null}} CargoPackage
 * @typedef {{pkg: string, dep_kinds: {kind: string | null}[]}} CargoEdge
 * @typedef {{id: string, deps: CargoEdge[], features: string[]}} CargoNode
 * @typedef {{packages: CargoPackage[], resolve: {root: string, nodes: CargoNode[]}}} CargoMetadata
 */

/**
 * Keep build dependencies and proc macros: generated code can carry their notices.
 * This is deliberately a source-graph superset, not a per-binary linker map.
 * @param {CargoMetadata} metadata
 */
export function nativePackages(metadata) {
  if (!Array.isArray(metadata?.packages) || !Array.isArray(metadata.resolve?.nodes)) {
    throw new Error("Cargo metadata has no resolved package graph.");
  }
  const packages = new Map(metadata.packages.map((pkg) => [pkg.id, pkg]));
  const nodes = new Map(metadata.resolve.nodes.map((node) => [node.id, node]));
  const root = metadata.resolve.root;
  if (!root || !packages.has(root) || !nodes.has(root)) throw new Error("Cargo root is missing.");
  const selected = new Set([root]);
  const queue = [root];
  for (let index = 0; index < queue.length; index++) {
    const node = nodes.get(queue[index]);
    if (!node || !Array.isArray(node.deps)) throw new Error("Cargo dependency node is missing.");
    for (const edge of node.deps) {
      if (
        !Array.isArray(edge.dep_kinds) ||
        edge.dep_kinds.length === 0 ||
        edge.dep_kinds.some(({ kind }) => ![null, "build", "dev"].includes(kind))
      ) {
        throw new Error("Cargo dependency kind needs review.");
      }
      if (!edge.dep_kinds.some(({ kind }) => kind !== "dev")) continue;
      if (!packages.has(edge.pkg)) throw new Error("Cargo dependency package is missing.");
      if (!selected.has(edge.pkg)) {
        selected.add(edge.pkg);
        queue.push(edge.pkg);
      }
    }
  }
  return [...selected]
    .filter((id) => id !== root)
    .map((id) => {
      const pkg = packages.get(id);
      if (
        !pkg ||
        !/^[a-zA-Z0-9_-]+$/u.test(pkg.name) ||
        !/^\d+\.\d+\.\d+(?:[-+][\w.-]+)?$/u.test(pkg.version) ||
        typeof pkg.license !== "string" ||
        !pkg.license.trim() ||
        typeof pkg.manifest_path !== "string"
      ) {
        throw new Error("Cargo package identity or license metadata needs review.");
      }
      return { ...pkg, features: [...(nodes.get(id)?.features ?? [])].sort() };
    })
    .sort((a, b) => `${a.name}@${a.version}`.localeCompare(`${b.name}@${b.version}`, "en"));
}

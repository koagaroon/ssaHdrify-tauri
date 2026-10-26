import { createHash } from "node:crypto";
import { join } from "node:path";

/** Small source fixtures exercise the production verifier without Cargo or network access. @param {string} directory */
export function sqliteBundleFixture(directory) {
  const sourceId = `2026-10-16 14:45:20 ${"a".repeat(64)}`;
  const strings = {
    SQLITE_VERSION: "3.54.1",
    SQLITE_SOURCE_ID: sourceId,
    SQLITE_SCM_BRANCH: "trunk",
    SQLITE_SCM_TAGS: "version-3.54.1",
    SQLITE_SCM_DATETIME: "2026-10-16T14:45:20.000",
  };
  const header =
    Object.entries(strings)
      .map(([name, value]) => `#define ${name} "${value}"`)
      .join("\n") + "\n#define SQLITE_VERSION_NUMBER 3054001\n";
  const bindings =
    Object.entries(strings)
      .map(([name, value]) => `pub const ${name}: &::core::ffi::CStr = c"${value}";`)
      .join("\n") + "\npub const SQLITE_VERSION_NUMBER: i32 = 3054001;\n";
  /** @type {Record<string, string>} */
  const files = {
    "Cargo.toml": '[package]\nname = "libsqlite3-sys"\nversion = "0.39.1"\n',
    "build.rs": "fn main() {}\n",
    LICENSE: "Fixture binding license\n",
    "sqlite3/sqlite3.c": "/* Tiny source fixture */\n",
    "sqlite3/sqlite3.h": header,
    "sqlite3/sqlite3ext.h": "/* Tiny extension header fixture */\n",
    "sqlite3/bindgen_bundled_version.rs": bindings,
    "sqlite3/bindgen_bundled_version_ext.rs": bindings,
  };
  const provenance = {
    schemaVersion: 1,
    crate: {
      name: "libsqlite3-sys",
      version: "0.39.1",
      source: "registry+https://github.com/rust-lang/crates.io-index",
      url: "https://static.crates.io/crates/libsqlite3-sys/libsqlite3-sys-0.39.1.crate",
      sha256: "b".repeat(64),
      features: ["bundled", "bundled_bindings", "cc"],
    },
    sqlite: {
      version: "3.54.1",
      sourceId,
      url: "https://sqlite.org/2026/sqlite-amalgamation-3540100.zip",
      sha3_256: "c".repeat(64),
      sourceSha3_256: createHash("sha3-256").update(files["sqlite3/sqlite3.c"]).digest("hex"),
    },
    files: Object.fromEntries(
      Object.entries(files).map(([path, text]) => [
        path,
        createHash("sha256").update(text).digest("hex"),
      ])
    ),
  };
  const pkg = {
    id: "sqlite",
    name: "libsqlite3-sys",
    version: "0.39.1",
    source: provenance.crate.source,
    manifest_path: join(directory, "Cargo.toml"),
    license: "MIT",
    license_file: null,
  };
  const node = { id: "sqlite", features: ["bundled", "bundled_bindings", "cc"], deps: [] };
  const lockText = `version = 4\n\n[[package]]\nname = "libsqlite3-sys"\nversion = "0.39.1"\nsource = "${pkg.source}"\nchecksum = "${provenance.crate.sha256}"\n`;
  return { provenance, lockText, package: pkg, node, files };
}

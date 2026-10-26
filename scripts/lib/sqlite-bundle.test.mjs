import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, test } from "vitest";
import {
  inspectSqliteBundle,
  selectSqliteBundle,
  sqliteBundlePath,
  verifySqliteBundle,
} from "./sqlite-bundle.mjs";
import { sqliteBundleFixture } from "./sqlite-bundle-fixture.mjs";

const projectRoot = fileURLToPath(new URL("../../", import.meta.url));

function fixture() {
  const directory = join(projectRoot, "test-sqlite-registry-source");
  const f = sqliteBundleFixture(directory);
  const metadata = {
    packages: [f.package],
    resolve: {
      root: "app",
      nodes: [
        {
          id: "app",
          features: [],
          deps: [{ pkg: "sqlite", dep_kinds: [{ kind: /** @type {string | null} */ (null) }] }],
        },
        f.node,
      ],
    },
  };
  /** @param {string} path */
  const readSource = (path) => {
    const name = relative(directory, path).replaceAll("\\", "/");
    if (!Object.hasOwn(f.files, name)) throw new Error("Missing fixture file");
    return Buffer.from(f.files[name]);
  };
  return { ...f, metadata, readSource };
}

describe("published SQLite bundle provenance", () => {
  test("binds the repository's reviewed published package to its lockfile without Cargo or network", () => {
    const provenance = JSON.parse(readFileSync(join(projectRoot, sqliteBundlePath), "utf8"));
    expect(inspectSqliteBundle(provenance)).toEqual(provenance);
    const f = fixture();
    f.metadata.packages[0].version = provenance.crate.version;
    f.metadata.resolve.nodes[1].features = provenance.crate.features;
    expect(
      selectSqliteBundle(
        f.metadata,
        provenance,
        readFileSync(join(projectRoot, "src-tauri/Cargo.lock"), "utf8")
      ).package.version
    ).toBe(provenance.crate.version);
  });

  test("verifies the selected package's C source, headers, bindings, build code, and license", () => {
    const f = fixture();
    expect(verifySqliteBundle(f)).toEqual({ package: f.package, sqlite: f.provenance.sqlite });
  });

  test.each([null, {}, { schemaVersion: 2 }])("rejects malformed provenance: %j", (provenance) => {
    expect(() => inspectSqliteBundle(provenance)).toThrow(/invalid schema/u);
  });

  test("rejects invalid crate, archive, and source identities", () => {
    const f = fixture();
    const wrongUrl = structuredClone(f.provenance);
    wrongUrl.crate.url = "https://example.invalid/archive.crate";
    const wrongCrateHash = structuredClone(f.provenance);
    wrongCrateHash.crate.sha256 = "short";
    const wrongSourceHash = structuredClone(f.provenance);
    wrongSourceHash.sqlite.sourceSha3_256 = "short";
    const wrongSourceUrl = structuredClone(f.provenance);
    wrongSourceUrl.sqlite.url = "https://sqlite.org/2026/sqlite-amalgamation-3540000.zip";
    const wrongSourceId = structuredClone(f.provenance);
    wrongSourceId.sqlite.sourceId = "missing source revision";
    for (const provenance of [
      wrongUrl,
      wrongCrateHash,
      wrongSourceHash,
      wrongSourceUrl,
      wrongSourceId,
    ]) {
      expect(() => verifySqliteBundle({ ...f, provenance })).toThrow(/invalid .*identity/u);
    }
  });

  test("rejects missing, duplicate, and non-registry packages", () => {
    const f = fixture();
    for (const packages of [
      [],
      [f.package, f.package],
      [{ ...f.package, source: null }],
      [{ ...f.package, version: "0.38.2" }],
    ]) {
      expect(() => verifySqliteBundle({ ...f, metadata: { ...f.metadata, packages } })).toThrow(
        /exactly one reviewed crates.io/u
      );
    }
  });

  test.each([
    { features: [] },
    { features: ["bundled"] },
    { features: ["bundled", "bundled_bindings", "cc", "sqlcipher"] },
    { features: ["bundled", "bundled_bindings", "cc", "buildtime_bindgen"] },
    { features: ["bundled", "bundled_bindings", "cc", "new-feature"] },
  ])("rejects an unbundled or unreviewed feature selection: $features", ({ features }) => {
    const f = fixture();
    f.metadata.resolve.nodes[1].features = features;
    expect(() => verifySqliteBundle(f)).toThrow(/ordinary bundled build/u);
  });

  test("requires SQLite on the application graph rather than a disconnected or dev-only node", () => {
    const f = fixture();
    f.metadata.resolve.nodes[0].deps = [];
    expect(() => verifySqliteBundle(f)).toThrow(/absent from the application/u);
    f.metadata.resolve.nodes[0].deps = [{ pkg: "sqlite", dep_kinds: [{ kind: "dev" }] }];
    expect(() => verifySqliteBundle(f)).toThrow(/absent from the application/u);
  });

  test("rejects changed lock checksums, sources, versions, and duplicate entries", () => {
    const f = fixture();
    for (const lockText of [
      f.lockText.replace(f.provenance.crate.sha256, "d".repeat(64)),
      f.lockText.replace(f.provenance.crate.source, "git+https://example.invalid/source"),
      f.lockText.replace('version = "0.39.1"', 'version = "0.38.2"'),
      f.lockText + f.lockText.slice(f.lockText.indexOf("[[package]]")),
      f.lockText.replace("checksum = ", "missing_checksum = "),
    ])
      expect(() => verifySqliteBundle({ ...f, lockText })).toThrow(/lock/u);
  });

  test("rejects removed and unexpected provenance file entries", () => {
    const f = fixture();
    for (const files of [
      { ...f.provenance.files, "../../private.txt": "a".repeat(64) },
      { LICENSE: f.provenance.files.LICENSE },
    ]) {
      expect(() => verifySqliteBundle({ ...f, provenance: { ...f.provenance, files } })).toThrow(
        /every reviewed source/u
      );
    }
  });

  test.each([
    "Cargo.toml",
    "build.rs",
    "LICENSE",
    "sqlite3/sqlite3.c",
    "sqlite3/sqlite3.h",
    "sqlite3/sqlite3ext.h",
    "sqlite3/bindgen_bundled_version.rs",
    "sqlite3/bindgen_bundled_version_ext.rs",
  ])("rejects changed selected-package bytes: %s", (file) => {
    const f = fixture();
    f.files[file] += "changed\n";
    expect(() => verifySqliteBundle(f)).toThrow(/differs from the reviewed package/u);
  });

  test("compares the C source against the official checksum independently of file hashes", () => {
    const f = fixture();
    f.provenance.sqlite.sourceSha3_256 = "a".repeat(64);
    expect(() => verifySqliteBundle(f)).toThrow(/official source checksum/u);
  });

  test("rejects missing selected-package source files", () => {
    const f = fixture();
    delete f.files["sqlite3/sqlite3.h"];
    expect(() => verifySqliteBundle(f)).toThrow(/Missing fixture file/u);
  });

  test("rejects a changed header identity even when its file hash is acknowledged", () => {
    const f = fixture();
    f.files["sqlite3/sqlite3.h"] = f.files["sqlite3/sqlite3.h"].replace("3.54.1", "3.54.0");
    f.provenance.files["sqlite3/sqlite3.h"] = createHash("sha256")
      .update(f.files["sqlite3/sqlite3.h"])
      .digest("hex");
    expect(() => verifySqliteBundle(f)).toThrow(/header identity/u);
  });

  test.each(["sqlite3/bindgen_bundled_version.rs", "sqlite3/bindgen_bundled_version_ext.rs"])(
    "rejects stale generated binding metadata even with its updated hash: %s",
    (file) => {
      const f = fixture();
      f.files[file] = f.files[file].replace("3054001", "3054000");
      f.provenance.files[file] = createHash("sha256").update(f.files[file]).digest("hex");
      expect(() => verifySqliteBundle(f)).toThrow(/binding version number/u);
    }
  );
});

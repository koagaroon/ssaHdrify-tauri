import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { afterEach, describe, it } from "vitest";
import { generateNativeNotices } from "../native-notices.mjs";
import {
  nativeSourcesPath,
  nativeTextPath,
  nativeManifestPath,
  textDigest,
} from "./native-notices.mjs";

/** @type {string[]} */
const temporary = [];
afterEach(() => {
  for (const path of temporary.splice(0)) rmSync(path, { recursive: true, force: true });
});

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "ssahdrify-native-generator-"));
  temporary.push(root);
  /** @param {string} path @param {string} text */
  function write(path, text) {
    mkdirSync(dirname(join(root, path)), { recursive: true });
    writeFileSync(join(root, path), text, "utf8");
  }
  for (const path of [
    "src-tauri/Cargo.lock",
    "src-tauri/Cargo.toml",
    "src-tauri/vendor/libsqlite3-sys/Cargo.toml",
    "src-tauri/vendor/libsqlite3-sys/LICENSE",
  ])
    write(path, "fixture\n");
  write("rust-toolchain.toml", '[toolchain]\nchannel = "1.0.0"\n');
  write("crate/LICENSE", "Fixture full license\n");
  mkdirSync(join(root, "src/assets/licenses/native-sources"), { recursive: true });
  /** @type {{schemaVersion: number, target: string, packages: Record<string, never>, supplements: {name: string, package: string, files: {path: string, url: string, sha256: string}[]}[]}} */
  const sources = {
    schemaVersion: 1,
    target: "x86_64-pc-windows-msvc",
    packages: {},
    supplements: [],
  };
  write(nativeSourcesPath, JSON.stringify(sources));
  /** @type {import("./native-notices.mjs").CargoMetadata} */
  const metadata = {
    packages: [
      {
        id: "root",
        name: "app",
        version: "1.0.0",
        license: "MIT",
        license_file: null,
        manifest_path: join(root, "src-tauri/Cargo.toml"),
        source: null,
      },
      {
        id: "library",
        name: "library",
        version: "1.0.0",
        license: "MIT",
        license_file: null,
        manifest_path: join(root, "crate/Cargo.toml"),
        source: "registry+https://github.com/rust-lang/crates.io-index",
      },
    ],
    resolve: {
      root: "root",
      nodes: [
        { id: "root", features: [], deps: [{ pkg: "library", dep_kinds: [{ kind: null }] }] },
        { id: "library", features: [], deps: [] },
      ],
    },
  };
  /** @param {string} command @param {string[]} args */
  const execute = (command, args) => {
    if (command === "rustc") return "release: 1.0.0\n";
    assert.equal(command, "cargo");
    assert.ok(args.includes("--locked"));
    assert.equal(args[args.indexOf("--filter-platform") + 1], "x86_64-pc-windows-msvc");
    return JSON.stringify(metadata);
  };
  return { root, write, sources, execute };
}

describe("native notice generator guards", () => {
  it("reproduces a complete inventory and refuses a removed or modified license", () => {
    const f = fixture();
    generateNativeNotices(f.root, "--write", f.execute);
    generateNativeNotices(f.root, "--check", f.execute);
    const original = readFileSync(join(f.root, nativeTextPath), "utf8");
    f.write("crate/LICENSE", "Changed legal text\n");
    assert.throws(() => generateNativeNotices(f.root, "--check", f.execute), /stale/u);
    assert.equal(readFileSync(join(f.root, nativeTextPath), "utf8"), original);
    rmSync(join(f.root, "crate/LICENSE"));
    f.write("crate/copying-phase.cc", "This is source code, not a license.");
    assert.throws(
      () => generateNativeNotices(f.root, "--write", f.execute),
      /No primary native license/u
    );
    assert.equal(readFileSync(join(f.root, nativeTextPath), "utf8"), original);
  });

  it("does not publish an old dependency graph under inputs changed during Cargo resolution", () => {
    const f = fixture();
    assert.throws(
      () =>
        generateNativeNotices(f.root, "--write", (command, args) => {
          const result = f.execute(command, args);
          if (command === "cargo")
            f.write("src-tauri/Cargo.lock", "changed while Cargo was running\n");
          return result;
        }),
      /inputs changed during generation/u
    );
    assert.ok(!existsSync(join(f.root, nativeTextPath)));
    assert.ok(!existsSync(join(f.root, nativeManifestPath)));
  });

  it("rejects a supplemental traversal even when the escaped file has the expected hash", () => {
    const f = fixture();
    const text = "Private fixture content must not be published.\n";
    f.write(".env", text);
    f.sources.supplements.push({
      name: "fixture",
      package: "library@1.0.0",
      files: [
        {
          path: "src/assets/licenses/native-sources/../../../../.env",
          url: "https://example.org/LICENSE",
          sha256: textDigest(text),
        },
      ],
    });
    f.write(nativeSourcesPath, JSON.stringify(f.sources));
    assert.throws(
      () => generateNativeNotices(f.root, "--write", f.execute),
      /leaves its source directory/u
    );
    assert.ok(!existsSync(join(f.root, nativeTextPath)));
  });

  it("rejects changed supplemental content and stale supplemental package versions", () => {
    for (const version of ["1.0.0", "0.9.0"]) {
      const f = fixture();
      f.write("src/assets/licenses/native-sources/extra.txt", "actual license text\n");
      f.sources.supplements.push({
        name: "fixture",
        package: `library@${version}`,
        files: [
          {
            path: "src/assets/licenses/native-sources/extra.txt",
            url: "https://example.org/LICENSE",
            sha256: textDigest("old license text\n"),
          },
        ],
      });
      f.write(nativeSourcesPath, JSON.stringify(f.sources));
      assert.throws(
        () => generateNativeNotices(f.root, "--write", f.execute),
        /Supplemental notice changed|supplemental component changed/u
      );
      assert.ok(!existsSync(join(f.root, nativeTextPath)));
    }
  });
});

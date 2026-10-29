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
import { sqliteBundleFixture } from "./sqlite-bundle-fixture.mjs";
import { sqliteBundlePath } from "./sqlite-bundle.mjs";

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
  write("src-tauri/Cargo.toml", "fixture\n");
  const sqlite = sqliteBundleFixture(join(root, "sqlite-crate"));
  write("src-tauri/Cargo.lock", sqlite.lockText);
  write(sqliteBundlePath, JSON.stringify(sqlite.provenance));
  for (const [path, text] of Object.entries(sqlite.files)) write(`sqlite-crate/${path}`, text);
  write("rust-toolchain.toml", '[toolchain]\nchannel = "1.0.0"\n');
  write("crate/LICENSE", "Fixture full license\n");
  mkdirSync(join(root, "src/assets/licenses/native-sources"), { recursive: true });
  /** @type {{schemaVersion: number, target: string, packages: Record<string, never>, supplements: {name: string, package: string, files: {path: string, url: string, sha256: string, format?: string}[]}[]}} */
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
      sqlite.package,
    ],
    resolve: {
      root: "root",
      nodes: [
        {
          id: "root",
          features: [],
          deps: [
            { pkg: "library", dep_kinds: [{ kind: null }] },
            { pkg: sqlite.package.id, dep_kinds: [{ kind: null }] },
          ],
        },
        { id: "library", features: [], deps: [] },
        sqlite.node,
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
  return { root, write, sources, metadata, sqlite, execute };
}

describe("native notice generator guards", () => {
  it("preserves encoded literal markup and decodes Rust license references only once", () => {
    const f = fixture();
    const source =
      "<h1>Copyright notices for The Rust Standard Library</h1>" +
      "<p>Copyright &copy; 2026 &#x1F642;</p>" +
      "<pre>&lt;license&gt; &amp;lt;literal&amp;gt;<br>Next line</pre>";
    const path = "src/assets/licenses/native-sources/rust-library.html";
    f.write(path, source);
    f.sources.supplements.push({
      name: "Rust license fixture",
      package: "library@1.0.0",
      files: [
        {
          path,
          url: "https://example.org/rust-library.html",
          sha256: textDigest(source),
          format: "rust-library-html",
        },
      ],
    });
    f.write(nativeSourcesPath, JSON.stringify(f.sources));

    generateNativeNotices(f.root, "--write", f.execute);
    generateNativeNotices(f.root, "--check", f.execute);
    const text = readFileSync(join(f.root, nativeTextPath), "utf8");
    assert.ok(
      text.includes(
        "Copyright notices for The Rust Standard Library\n" +
          "Copyright © 2026 🙂\n<license> &lt;literal&gt;\nNext line\n"
      )
    );
    assert.ok(!text.includes("<h1>"));
    assert.ok(!text.includes("<pre>"));
  });

  it("includes the published SQLite binding license with its exact registry source", () => {
    const f = fixture();
    generateNativeNotices(f.root, "--write", f.execute);
    generateNativeNotices(f.root, "--check", f.execute);
    const text = readFileSync(join(f.root, nativeTextPath), "utf8");
    assert.ok(text.includes(f.sqlite.files.LICENSE));
    assert.ok(
      text.includes(`Source: https://crates.io/crates/libsqlite3-sys/${f.sqlite.package.version}`)
    );
    assert.ok(!text.includes("local SQLite override"));
    const manifest = JSON.parse(readFileSync(join(f.root, nativeManifestPath), "utf8"));
    assert.equal(
      manifest.inputs[sqliteBundlePath],
      textDigest(JSON.stringify(f.sqlite.provenance))
    );
  });

  it("rejects native path or Git sources before writing an inventory", () => {
    for (const source of [null, "git+https://example.org/library#review-required"]) {
      const f = fixture();
      f.metadata.packages[1].source = source;
      assert.throws(
        () => generateNativeNotices(f.root, "--write", f.execute),
        /Review new native dependency source/u
      );
      assert.ok(!existsSync(join(f.root, nativeTextPath)));
    }
  });

  it("refuses changed installed SQLite bytes before writing a notice snapshot", () => {
    const f = fixture();
    f.write("sqlite-crate/sqlite3/sqlite3.c", "unreviewed SQLite source\n");
    assert.throws(
      () => generateNativeNotices(f.root, "--write", f.execute),
      /SQLite bundle source differs from the reviewed package/u
    );
    assert.ok(!existsSync(join(f.root, nativeTextPath)));
    assert.ok(!existsSync(join(f.root, nativeManifestPath)));
  });

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
            f.write("src-tauri/Cargo.toml", "changed while Cargo was running\n");
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

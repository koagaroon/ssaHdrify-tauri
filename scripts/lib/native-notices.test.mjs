import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, it } from "vitest";
import {
  buildNativeNotices,
  isNativeLicenseFile,
  nativeInputs,
  nativeManifestPath,
  nativePackages,
  nativeSourcesPath,
  nativeTarget,
  nativeTextPath,
  textDigest,
} from "./native-notices.mjs";

/** @type {string[]} */
const temporary = [];
afterEach(() => {
  for (const path of temporary.splice(0)) rmSync(path, { recursive: true, force: true });
});

/** @param {string} name @param {string | null} [license] */
function pkg(name, license = "MIT") {
  return {
    id: name,
    name,
    version: "1.0.0",
    license,
    license_file: null,
    manifest_path: `${name}/Cargo.toml`,
    source: null,
  };
}
/** @param {string} id @param {import("./native-notices.mjs").CargoEdge[]} [deps] */
const node = (id, deps = []) => ({ id, deps, features: [] });
/** @param {string} pkg @param {string | null} [kind] */
const edge = (pkg, kind = null) => ({ pkg, dep_kinds: [{ kind }] });

describe("locked native notice coverage", () => {
  it("collects legal documents without mistaking copying algorithms for license text", () => {
    for (const name of [
      "LICENSE",
      "LICENSE-APACHE-2.0",
      "COPYING.LESSER",
      "NOTICE.txt",
      "COPYRIGHT.md",
      "LICENSE-UNICODE",
    ])
      assert.ok(isNativeLicenseFile(name), name);
    for (const name of [
      "copying-phase.cc",
      "copying-phase.h",
      "license.rs",
      "license.test.ts",
      "README.md",
    ])
      assert.ok(!isNativeLicenseFile(name), name);
  });
  it("keeps generated-code dependencies and omits dependencies reachable only through tests", () => {
    const metadata = {
      packages: [pkg("app"), pkg("runtime"), pkg("generator"), pkg("shared"), pkg("test-only")],
      resolve: {
        root: "app",
        nodes: [
          node("app", [edge("runtime"), edge("generator", "build"), edge("test-only", "dev")]),
          node("runtime", [edge("shared")]),
          node("generator", [edge("shared")]),
          node("shared", [edge("runtime")]),
          node("test-only"),
        ],
      },
    };
    assert.deepEqual(
      nativePackages(metadata).map(({ name }) => name),
      ["generator", "runtime", "shared"]
    );
    const withoutBuild = structuredClone(metadata);
    withoutBuild.resolve.nodes[0].deps = withoutBuild.resolve.nodes[0].deps.filter(
      ({ pkg }) => pkg !== "generator"
    );
    assert.ok(!nativePackages(withoutBuild).some(({ name }) => name === "generator"));
  });

  it("refuses missing graph nodes, unknown dependency kinds, and undeclared licenses", () => {
    const metadata = {
      packages: [pkg("app"), pkg("library")],
      resolve: { root: "app", nodes: [node("app", [edge("library")]), node("library")] },
    };
    for (const broken of [
      { ...metadata, packages: [pkg("app")] },
      { ...metadata, packages: [pkg("app"), pkg("library", null)] },
      {
        ...metadata,
        resolve: {
          root: "app",
          nodes: [node("app", [edge("library", "future-kind")]), node("library")],
        },
      },
      { ...metadata, resolve: { root: "app", nodes: [node("app", [edge("library")])] } },
    ])
      assert.throws(() => nativePackages(broken), /missing|needs review/u);
  });

  function fixture() {
    const root = mkdtempSync(join(tmpdir(), "ssahdrify-notices-"));
    temporary.push(root);
    for (const path of [
      "src-tauri/Cargo.lock",
      "src-tauri/Cargo.toml",
      "rust-toolchain.toml",
      "src-tauri/vendor/libsqlite3-sys/Cargo.toml",
      "src-tauri/vendor/libsqlite3-sys/LICENSE",
      nativeSourcesPath,
    ]) {
      mkdirSync(dirname(join(root, path)), { recursive: true });
      writeFileSync(join(root, path), `fixture: ${path}\n`, "utf8");
    }
    const text = "complete fixture license\n";
    mkdirSync(dirname(join(root, nativeTextPath)), { recursive: true });
    writeFileSync(join(root, nativeTextPath), text, "utf8");
    const manifest = {
      schemaVersion: 1,
      target: nativeTarget,
      inputs: nativeInputs(root),
      sha256: textDigest(text),
    };
    writeFileSync(join(root, nativeManifestPath), JSON.stringify(manifest), "utf8");
    return { root, text };
  }

  it("requires no Cargo or network for an unchanged offline notice snapshot", () => {
    const { root, text } = fixture();
    assert.equal(buildNativeNotices(root), text);
    writeFileSync(
      join(root, "src-tauri/Cargo.lock"),
      readFileSync(join(root, "src-tauri/Cargo.lock"), "utf8").replace(/\n/gu, "\r\n"),
      "utf8"
    );
    assert.equal(buildNativeNotices(root), text);
  });

  it("rejects changed locks, compiler pins, source provenance, local overrides, or notice text", () => {
    for (const path of [
      "src-tauri/Cargo.lock",
      "src-tauri/Cargo.toml",
      "rust-toolchain.toml",
      "src-tauri/vendor/libsqlite3-sys/LICENSE",
      nativeSourcesPath,
      nativeTextPath,
    ]) {
      const { root } = fixture();
      writeFileSync(join(root, path), "changed\n", "utf8");
      assert.throws(() => buildNativeNotices(root), /stale/u);
    }
    const { root } = fixture();
    mkdirSync(join(root, ".cargo"));
    writeFileSync(join(root, ".cargo/config.toml"), "[build]\n", "utf8");
    assert.throws(() => buildNativeNotices(root), /stale/u);
  });

  it("includes native engines, nested notices, copied portions, SDK and standard-library texts", () => {
    const root = fileURLToPath(new URL("../../", import.meta.url));
    const text = buildNativeNotices(root);
    for (const expected of [
      "deno_core 0.413.0",
      "v8 150.4.0",
      "dwrote 0.11.5",
      "regex-syntax 0.8.11",
      "src/unicode_tables/LICENSE-UNICODE",
      "vendor/woff2/LICENSE",
      "src/spin/LICENSE",
      "Copyright Joyent, Inc. and other Node contributors.",
      "HarfBuzz portions",
      "WebView2 SDK 1.0.3800.47",
      "Copyright notices for The Rust Standard Library",
      "GNU LESSER GENERAL PUBLIC LICENSE",
      "V8 pinned submodule notices",
    ])
      assert.ok(text.includes(expected), `Missing native notice: ${expected}`);
    assert.ok(!text.includes(root));
    assert.ok(!text.includes("file:///"));
  });
});

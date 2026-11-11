import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, it } from "vitest";
import { buildFrontendNotices, collectNpmNotices } from "./frontend-notices.mjs";

/** @type {import("./frontend-notices.mjs").NoticeLockfile} */
const lockfile = {
  lockfileVersion: 3,
  packages: {
    "": { dependencies: { react: "^19.2.8" } },
    "node_modules/react": { version: "19.2.8", license: "MIT" },
    "node_modules/scheduler": { version: "0.27.0", license: "MIT" },
    "node_modules/esbuild": { version: "0.28.2", license: "MIT", dev: true },
    "node_modules/vite": {
      version: "8.3.8",
      license: "MIT",
      dev: true,
      dependencies: { rolldown: "~1.2.18" },
    },
    "node_modules/rolldown": { version: "1.2.18", license: "MIT", dev: true },
    "node_modules/vitest": { version: "5.0.0", license: "MIT", dev: true },
  },
};
/**
 * @param {string} location
 * @param {import("./frontend-notices.mjs").NoticeLockfile} [source]
 */
function readPackage(location, source = lockfile) {
  const entry = source.packages[location];
  const name = location.slice(location.lastIndexOf("node_modules/") + "node_modules/".length);
  const files = [{ name: "LICENSE", text: "Copyright: fixture\nPermission: fixture\n" }];
  if (name === "rolldown") {
    files.push({ name: "THIRD-PARTY-LICENSE", text: "Full upstream attribution: fixture\n" });
  }
  return {
    metadata: { name, ...entry },
    files,
  };
}

describe("frontend runtime notice inventory", () => {
  it("includes runtime packages and Vite/Rolldown/esbuild helpers but excludes test tools", () => {
    const entries = collectNpmNotices(lockfile, readPackage);
    assert.deepEqual(
      entries.map(({ name }) => name),
      ["esbuild", "react", "rolldown", "scheduler", "vite"]
    );
  });

  it("uses Vite's nested Rolldown instead of an unrelated hoisted version", () => {
    const nested = structuredClone(lockfile);
    nested.packages["node_modules/rolldown"].version = "2.0.0";
    nested.packages["node_modules/vite/node_modules/rolldown"] = {
      version: "1.2.19",
      license: "MIT",
      dev: true,
    };
    const entries = collectNpmNotices(nested, (location) => readPackage(location, nested));
    const rolldown = entries.filter(({ name }) => name === "rolldown");
    assert.equal(rolldown.length, 1);
    assert.equal(rolldown[0].version, "1.2.19");
    assert.equal(rolldown[0].location, "node_modules/vite/node_modules/rolldown");
    assert.deepEqual(
      rolldown[0].files.map(({ name }) => name),
      ["LICENSE", "THIRD-PARTY-LICENSE"]
    );
  });

  it("refuses missing helper locks and an absent or unfamiliar Vite dependency requirement", () => {
    for (const location of ["node_modules/vite", "node_modules/rolldown", "node_modules/esbuild"]) {
      const broken = structuredClone(lockfile);
      delete broken.packages[location];
      assert.throws(() => collectNpmNotices(broken, readPackage), /missing from the lock/u);
    }
    /** @type {Record<string, string>[]} */
    const requirements = [{}, { rolldown: "^1.2.18" }];
    for (const dependencies of requirements) {
      const broken = structuredClone(lockfile);
      broken.packages["node_modules/vite"].dependencies = dependencies;
      assert.throws(() => collectNpmNotices(broken, readPackage), /dependency requirement/u);
    }
  });

  it("refuses missing esbuild license text and a stale installed helper version", () => {
    for (const invalid of ["license", "version"]) {
      assert.throws(
        () =>
          collectNpmNotices(lockfile, (location) => {
            const entry = readPackage(location);
            if (location === "node_modules/esbuild") {
              if (invalid === "license") entry.files = [];
              else entry.metadata.version = "0.28.1";
            }
            return entry;
          }),
        /No full runtime license text|differs from the lock/u
      );
    }
  });

  it("refuses incompatible or prerelease Rolldown locks without falling back past a nested entry", () => {
    for (const version of ["1.2.17", "1.3.0", "2.2.18", "1.2.19-beta.1"]) {
      const broken = structuredClone(lockfile);
      broken.packages["node_modules/vite/node_modules/rolldown"] = {
        version,
        license: "MIT",
        dev: true,
      };
      assert.throws(() => collectNpmNotices(broken, readPackage), /does not satisfy Vite/u);
    }
  });

  it("refuses stale Rolldown identity, version, license and Vite dependency metadata", () => {
    for (const replacement of [
      { name: "unrelated" },
      { version: "1.2.17" },
      { license: "unknown" },
    ]) {
      assert.throws(
        () =>
          collectNpmNotices(lockfile, (location) => {
            const entry = readPackage(location);
            if (location === "node_modules/rolldown") Object.assign(entry.metadata, replacement);
            return entry;
          }),
        /differs from the lock|license metadata/u
      );
    }
    assert.throws(
      () =>
        collectNpmNotices(lockfile, (location) => {
          const entry = readPackage(location);
          if (location === "node_modules/vite")
            entry.metadata.dependencies = { rolldown: "~1.3.0" };
          return entry;
        }),
      /Vite's Rolldown dependency differs/u
    );
  });

  it("requires both full Rolldown license texts and refuses an empty supplemental notice", () => {
    for (const absent of ["LICENSE", "THIRD-PARTY-LICENSE"]) {
      assert.throws(
        () =>
          collectNpmNotices(lockfile, (location) => {
            const entry = readPackage(location);
            if (location === "node_modules/rolldown") {
              entry.files = entry.files.filter(({ name }) => name !== absent);
            }
            return entry;
          }),
        /No full runtime license text|Missing Rolldown runtime notice/u
      );
    }
    assert.throws(
      () =>
        collectNpmNotices(lockfile, (location) => {
          const entry = readPackage(location);
          if (location === "node_modules/rolldown") entry.files[1].text = " \n";
          return entry;
        }),
      /Empty runtime notice/u
    );
  });

  it("refuses an untracked nested installation that shadows the locked hoisted Rolldown", () => {
    const root = mkdtempSync(join(tmpdir(), "frontend-notices-"));
    try {
      const nested = join(root, "node_modules/vite/node_modules/rolldown");
      mkdirSync(nested, { recursive: true });
      writeFileSync(join(root, "package-lock.json"), JSON.stringify(lockfile), "utf8");
      writeFileSync(
        join(nested, "package.json"),
        JSON.stringify({ name: "rolldown", version: "1.2.18" }),
        "utf8"
      );
      assert.throws(() => buildFrontendNotices(root), /resolves a different Rolldown location/u);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("refuses a missing production lock entry instead of shipping a partial inventory", () => {
    const broken = structuredClone(lockfile);
    delete broken.packages["node_modules/react"];
    assert.throws(() => collectNpmNotices(broken, readPackage), /missing from the lock/u);
  });

  it("refuses stale installed versions and mismatched license metadata", () => {
    for (const replacement of [{ version: "19.2.7" }, { license: "unknown" }]) {
      assert.throws(
        () =>
          collectNpmNotices(lockfile, (location) => {
            const entry = readPackage(location);
            Object.assign(entry.metadata, replacement);
            return entry;
          }),
        /differs from the lock|license metadata/u
      );
    }
  });

  it("requires a license file and refuses empty notice files", () => {
    for (const files of [[], [{ name: "NOTICE", text: "MIT" }], [{ name: "LICENSE", text: "" }]]) {
      assert.throws(
        () => collectNpmNotices(lockfile, (location) => ({ ...readPackage(location), files })),
        /No full runtime license text|Empty runtime notice/u
      );
    }
  });

  it("preserves additional NOTICE files along with the license", () => {
    const files = [
      { name: "LICENSE", text: "full original license" },
      { name: "NOTICE", text: "additional copyright attribution" },
    ];
    assert.deepEqual(
      collectNpmNotices(lockfile, (location) =>
        location === "node_modules/react"
          ? { ...readPackage(location), files }
          : readPackage(location)
      ).find(({ name }) => name === "react")?.files,
      files
    );
  });

  it("deterministically retains the current full runtime and asset license texts", () => {
    const root = fileURLToPath(new URL("../../", import.meta.url));
    const output = buildFrontendNotices(root);
    const currentLock = JSON.parse(
      readFileSync(new URL("../../package-lock.json", import.meta.url), "utf8")
    );
    const rolldownLocation = Object.hasOwn(
      currentLock.packages,
      "node_modules/vite/node_modules/rolldown"
    )
      ? "node_modules/vite/node_modules/rolldown"
      : "node_modules/rolldown";
    assert.equal(output, buildFrontendNotices(root));
    for (const file of [
      "LICENSE",
      "node_modules/react/LICENSE",
      "node_modules/react-dom/LICENSE",
      "node_modules/scheduler/LICENSE",
      "node_modules/@tauri-apps/api/LICENSE-MIT",
      "node_modules/@tauri-apps/api/LICENSE-APACHE-2.0",
      "node_modules/vite/LICENSE.md",
      "node_modules/esbuild/LICENSE.md",
      `${rolldownLocation}/LICENSE`,
      `${rolldownLocation}/THIRD-PARTY-LICENSE`,
      "src/assets/fonts/inter/LICENSE.txt",
      "src/assets/fonts/smiley-sans/LICENSE.txt",
      "src/assets/licenses/feather-LICENSE.txt",
      "src/assets/licenses/unicode-normalization-LICENSE-MIT.txt",
      "src/assets/licenses/unicode-normalization-LICENSE-UNICODE.txt",
      "src/assets/licenses/native-notices.txt",
    ]) {
      const source = readFileSync(new URL(`../../${file}`, import.meta.url), "utf8").replace(
        /\r\n/gu,
        "\n"
      );
      assert.ok(output.includes(source), `Missing full text: ${file}`);
    }
    for (const name of [
      "react",
      "react-dom",
      "scheduler",
      "@tauri-apps/api",
      "vite",
      "rolldown",
      "esbuild",
    ]) {
      assert.ok(output.includes(`Source: https://www.npmjs.com/package/${name}/v/`));
    }
    assert.ok(!output.includes(root));
  });
});

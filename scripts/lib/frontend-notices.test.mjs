import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
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
    "node_modules/vite": { version: "8.2.2", license: "MIT", dev: true },
    "node_modules/vitest": { version: "5.0.0", license: "MIT", dev: true },
  },
};
/** @param {string} location */
function readPackage(location) {
  const entry = lockfile.packages[location];
  return {
    metadata: { name: location.replace("node_modules/", ""), ...entry },
    files: [{ name: "LICENSE", text: "Copyright: fixture\nPermission: fixture\n" }],
  };
}

describe("frontend runtime notice inventory", () => {
  it("includes transitive runtime packages and Vite's emitted helper but excludes test tools", () => {
    const entries = collectNpmNotices(lockfile, readPackage);
    assert.deepEqual(
      entries.map(({ name }) => name),
      ["react", "scheduler", "vite"]
    );
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
      collectNpmNotices(lockfile, (location) => ({ ...readPackage(location), files }))[0].files,
      files
    );
  });

  it("deterministically retains the current full runtime and asset license texts", () => {
    const root = fileURLToPath(new URL("../../", import.meta.url));
    const output = buildFrontendNotices(root);
    assert.equal(output, buildFrontendNotices(root));
    for (const file of [
      "LICENSE",
      "node_modules/react/LICENSE",
      "node_modules/react-dom/LICENSE",
      "node_modules/scheduler/LICENSE",
      "node_modules/@tauri-apps/api/LICENSE_MIT",
      "node_modules/@tauri-apps/api/LICENSE_APACHE-2.0",
      "node_modules/vite/LICENSE.md",
      "src/assets/fonts/inter/LICENSE.txt",
      "src/assets/fonts/smiley-sans/LICENSE.txt",
      "src/assets/licenses/feather-LICENSE.txt",
    ]) {
      const source = readFileSync(new URL(`../../${file}`, import.meta.url), "utf8").replace(
        /\r\n/gu,
        "\n"
      );
      assert.ok(output.includes(source), `Missing full text: ${file}`);
    }
    for (const name of ["react", "react-dom", "scheduler", "@tauri-apps/api", "vite"]) {
      assert.ok(output.includes(`Source: https://www.npmjs.com/package/${name}/v/`));
    }
    assert.ok(!output.includes(root));
  });
});

import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { describe, it } from "vitest";
import { defaultToolchains, verifyTypeScriptToolchains } from "../verify-typescript-toolchains.mjs";

/**
 * @param {string} root
 * @param {{ name: string, version: string, bin?: Record<string, string>, createBin?: boolean }} pkg
 */
function writeToolchainFixture(root, pkg) {
  const pkgPath = join(root, "package.json");
  writeFileSync(
    pkgPath,
    JSON.stringify({
      name: pkg.name,
      version: pkg.version,
      bin: pkg.bin,
    }),
    "utf8"
  );
  if (pkg.createBin !== false && pkg.bin) {
    for (const rel of Object.values(pkg.bin)) {
      const target = join(root, rel);
      mkdirSync(join(target, ".."), { recursive: true });
      writeFileSync(target, "#!/usr/bin/env node\n", "utf8");
    }
  }
  return pathToFileURL(pkgPath);
}

describe("verifyTypeScriptToolchains", () => {
  it("verifies the active repository TypeScript 7 release and TypeScript 6 compatibility toolchains", async () => {
    /** @type {string[]} */
    const lines = [];
    await verifyTypeScriptToolchains(defaultToolchains, (line) => lines.push(line));
    assert.equal(lines.length, 2);
    assert.match(lines[0] ?? "", /^TypeScript release: 7\./u);
    assert.match(lines[1] ?? "", /^TypeScript API compatibility: wrapper 6\..*, engine 6\./u);
  });

  it("rejects wrong package names, missing binaries, wrong compiler major, and wrong engine major", async () => {
    const dir = mkdtempSync(join(tmpdir(), "ts-toolchain-test-"));
    try {
      const validUrl = writeToolchainFixture(dir, {
        name: "@typescript/typescript6",
        version: "6.0.2",
        bin: { tsc6: "./bin/tsc6" },
      });
      const base = {
        label: "API compatibility",
        packageUrl: validUrl,
        expectedPackageName: "@typescript/typescript6",
        expectedBinName: "tsc6",
        expectedBinPath: "bin/tsc6",
        expectedMajor: 6,
        getEffectiveVersion: () => "6.0.3",
        expectedEffectiveMajor: 6,
      };

      await assert.rejects(
        () => verifyTypeScriptToolchains([{ ...base, expectedPackageName: "wrong-pkg" }], () => {}),
        /Expected the API compatibility compiler package/u
      );

      await assert.rejects(
        () => verifyTypeScriptToolchains([{ ...base, expectedMajor: 7 }], () => {}),
        /Expected the API compatibility TypeScript compiler to be major 7/u
      );

      await assert.rejects(
        () =>
          verifyTypeScriptToolchains([{ ...base, getEffectiveVersion: () => "5.9.3" }], () => {}),
        /Expected the API compatibility TypeScript engine to be major 6/u
      );

      const missingBinDir = mkdtempSync(join(tmpdir(), "ts-toolchain-nobin-"));
      try {
        const noBinUrl = writeToolchainFixture(missingBinDir, {
          name: "typescript",
          version: "7.0.2",
          bin: { tsc: "./bin/tsc" },
          createBin: false,
        });
        await assert.rejects(() =>
          verifyTypeScriptToolchains(
            [
              {
                label: "release",
                packageUrl: noBinUrl,
                expectedPackageName: "typescript",
                expectedBinName: "tsc",
                expectedBinPath: "bin/tsc",
                expectedMajor: 7,
                getEffectiveVersion: () => undefined,
                expectedEffectiveMajor: null,
              },
            ],
            () => {}
          )
        );
      } finally {
        rmSync(missingBinDir, { recursive: true, force: true });
      }
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});

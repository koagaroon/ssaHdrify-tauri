import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);

const toolchains = [
  {
    label: "release",
    packageUrl: new URL("../node_modules/@typescript/native/package.json", import.meta.url),
    expectedPackageName: "typescript",
    expectedBinName: "tsc",
    expectedBinPath: "bin/tsc",
    expectedMajor: 7,
    getEffectiveVersion: () => undefined,
    expectedEffectiveMajor: null,
  },
  {
    label: "API compatibility",
    packageUrl: new URL("../node_modules/typescript/package.json", import.meta.url),
    expectedPackageName: "@typescript/typescript6",
    expectedBinName: "tsc6",
    expectedBinPath: "bin/tsc6",
    expectedMajor: 6,
    getEffectiveVersion: () => require("typescript").version,
    expectedEffectiveMajor: 6,
  },
];

for (const toolchain of toolchains) {
  const packageJson = JSON.parse(await readFile(toolchain.packageUrl, "utf8"));
  const version = packageJson.version;
  const major = Number.parseInt(version, 10);
  const compilerBin = packageJson.bin?.[toolchain.expectedBinName];
  const normalizedCompilerBin =
    typeof compilerBin === "string" ? compilerBin.replace(/^\.\//u, "") : "";

  if (
    packageJson.name !== toolchain.expectedPackageName ||
    normalizedCompilerBin !== toolchain.expectedBinPath
  ) {
    throw new Error(
      `Expected the ${toolchain.label} compiler package to be ${toolchain.expectedPackageName} with ${toolchain.expectedBinName} at ${toolchain.expectedBinPath}, got ${String(packageJson.name)} with ${String(compilerBin)}`
    );
  }

  await readFile(new URL(compilerBin, toolchain.packageUrl));

  if (major !== toolchain.expectedMajor) {
    throw new Error(
      `Expected the ${toolchain.label} TypeScript compiler to be major ${toolchain.expectedMajor}, got ${String(version)}`
    );
  }

  const effectiveVersion = toolchain.getEffectiveVersion();

  if (toolchain.expectedEffectiveMajor !== null) {
    const effectiveMajor =
      typeof effectiveVersion === "string" ? Number.parseInt(effectiveVersion, 10) : Number.NaN;

    if (effectiveMajor !== toolchain.expectedEffectiveMajor) {
      throw new Error(
        `Expected the ${toolchain.label} TypeScript engine to be major ${toolchain.expectedEffectiveMajor}, got ${String(effectiveVersion)}`
      );
    }
  }

  const versionSummary =
    effectiveVersion === undefined ? version : `wrapper ${version}, engine ${effectiveVersion}`;
  console.log(`TypeScript ${toolchain.label}: ${versionSummary}`);
}

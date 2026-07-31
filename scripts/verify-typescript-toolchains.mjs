import { readFile } from "node:fs/promises";

const toolchains = [
  {
    label: "release",
    packageUrl: new URL("../node_modules/typescript/package.json", import.meta.url),
    expectedMajor: 6,
  },
  {
    label: "pilot",
    packageUrl: new URL("../node_modules/@typescript/native/package.json", import.meta.url),
    expectedMajor: 7,
  },
];

for (const toolchain of toolchains) {
  const packageJson = JSON.parse(await readFile(toolchain.packageUrl, "utf8"));
  const version = packageJson.version;
  const major = Number.parseInt(version, 10);

  if (major !== toolchain.expectedMajor) {
    throw new Error(
      `Expected the ${toolchain.label} TypeScript compiler to be major ${toolchain.expectedMajor}, got ${String(version)}`
    );
  }

  console.log(`TypeScript ${toolchain.label}: ${version}`);
}

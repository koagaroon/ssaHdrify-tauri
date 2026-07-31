import { readFile } from "node:fs/promises";

const registryBaseUrl = "https://registry.npmjs.org";
const lockfile = JSON.parse(
  await readFile(new URL("../package-lock.json", import.meta.url), "utf8")
);

const targets = [
  {
    label: "TypeScript 7 release compiler",
    registryPackage: "typescript",
    lockPath: "node_modules/@typescript/native",
    expectedPackageName: "typescript",
    stableMajor: null,
  },
  {
    label: "TypeScript 6 API compatibility wrapper",
    registryPackage: "@typescript/typescript6",
    lockPath: "node_modules/typescript",
    expectedPackageName: "@typescript/typescript6",
    stableMajor: null,
  },
  {
    label: "TypeScript 6 effective API/compiler",
    registryPackage: "typescript",
    lockPath: "node_modules/@typescript/old",
    expectedPackageName: "typescript",
    stableMajor: 6,
  },
];

const staleTargets = [];
const stableVersionPattern = /^(\d+)\.(\d+)\.(\d+)$/u;
const stableVersionCollator = new Intl.Collator("en", { numeric: true });

for (const target of targets) {
  const lockedPackage = lockfile.packages?.[target.lockPath];

  if (
    lockedPackage?.name !== target.expectedPackageName ||
    typeof lockedPackage.version !== "string"
  ) {
    throw new Error(
      `Expected ${target.lockPath} to contain ${target.expectedPackageName} with a version`
    );
  }

  const packagePath = encodeURIComponent(target.registryPackage);
  const metadataPath = target.stableMajor === null ? `${packagePath}/latest` : packagePath;
  const response = await fetch(`${registryBaseUrl}/${metadataPath}`, {
    headers: {
      Accept:
        target.stableMajor === null ? "application/json" : "application/vnd.npm.install-v1+json",
    },
    signal: AbortSignal.timeout(15_000),
  });

  if (!response.ok) {
    throw new Error(`npm registry returned ${response.status} for ${target.registryPackage}`);
  }

  const metadata = await response.json();
  let latestVersion;

  if (typeof metadata !== "object" || metadata === null) {
    throw new Error(`npm registry returned invalid metadata for ${target.registryPackage}`);
  }

  if (target.stableMajor === null) {
    latestVersion = "version" in metadata ? metadata.version : undefined;
  } else {
    const versions = "versions" in metadata ? metadata.versions : undefined;

    if (typeof versions !== "object" || versions === null) {
      throw new Error(`npm registry returned no versions for ${target.registryPackage}`);
    }

    latestVersion = Object.keys(versions)
      .filter((version) => {
        const match = stableVersionPattern.exec(version);
        return match !== null && Number(match[1]) === target.stableMajor;
      })
      .sort(stableVersionCollator.compare)
      .at(-1);
  }

  if (typeof latestVersion !== "string" || !stableVersionPattern.test(latestVersion)) {
    throw new Error(
      `npm registry returned an invalid stable version for ${target.registryPackage}`
    );
  }

  console.log(`${target.label}: locked ${lockedPackage.version}, latest ${latestVersion}`);

  if (lockedPackage.version !== latestVersion) {
    staleTargets.push(`${target.registryPackage} ${lockedPackage.version} -> ${latestVersion}`);
  }
}

if (staleTargets.length > 0) {
  throw new Error(`TypeScript alias updates require manual review:\n${staleTargets.join("\n")}`);
}

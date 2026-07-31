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
  },
  {
    label: "TypeScript 6 API compatibility wrapper",
    registryPackage: "@typescript/typescript6",
    lockPath: "node_modules/typescript",
    expectedPackageName: "@typescript/typescript6",
  },
];

const staleTargets = [];

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
  const response = await fetch(`${registryBaseUrl}/${packagePath}/latest`, {
    headers: { Accept: "application/json" },
    signal: AbortSignal.timeout(15_000),
  });

  if (!response.ok) {
    throw new Error(`npm registry returned ${response.status} for ${target.registryPackage}`);
  }

  const metadata = await response.json();
  const latestVersion =
    typeof metadata === "object" && metadata !== null && "version" in metadata
      ? metadata.version
      : undefined;

  if (typeof latestVersion !== "string" || !/^\d+\.\d+\.\d+$/u.test(latestVersion)) {
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

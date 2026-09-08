import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

/**
 * @typedef {{name?: string, version?: string, license?: string, dev?: boolean, dependencies?: Record<string, string>}} PackageMetadata
 * @typedef {{lockfileVersion: number, packages: Record<string, PackageMetadata>}} NoticeLockfile
 * @typedef {{name: string, text: string}} NoticeFile
 * @typedef {{metadata: PackageMetadata, files: NoticeFile[]}} InstalledPackage
 */

/** @param {string} text */
const normalizeNewlines = (text) => text.replace(/\r\n/gu, "\n");
const licenseName = /^(?:licen[cs]e|copying)(?:$|[._-])/iu;
const noticeName = /^(?:licen[cs]e|copying|notice)(?:$|[._-])/iu;

/**
 * @param {NoticeLockfile} lockfile
 * @param {(location: string) => InstalledPackage} readPackage
 */
export function collectNpmNotices(lockfile, readPackage) {
  if (lockfile.lockfileVersion !== 3 || !lockfile.packages?.[""]?.dependencies) {
    throw new Error("Runtime notices require an npm v3 lockfile with root dependencies.");
  }
  for (const name of Object.keys(lockfile.packages[""].dependencies)) {
    const entry = lockfile.packages[`node_modules/${name}`];
    if (!entry || entry.dev)
      throw new Error(`Runtime dependency is missing from the lock: ${name}`);
  }
  const locations = Object.keys(lockfile.packages)
    .filter(
      (location) =>
        location !== "" &&
        // Vite's modulepreload helper is emitted into index.html builds.
        (!lockfile.packages[location].dev || location === "node_modules/vite")
    )
    .sort();
  if (!locations.includes("node_modules/vite")) {
    throw new Error("Vite runtime helper notices are missing from the lock.");
  }
  return locations.map((location) => {
    if (!/^node_modules\/(?:[A-Za-z0-9@_.-]+\/)*[A-Za-z0-9_.-]+$/u.test(location)) {
      throw new Error(`Unsupported runtime package location: ${location}`);
    }
    const { metadata, files } = readPackage(location);
    const locked = lockfile.packages[location];
    const name = location.slice(location.lastIndexOf("node_modules/") + "node_modules/".length);
    if (metadata.name !== name || metadata.version !== locked.version) {
      throw new Error(`Installed runtime package differs from the lock: ${location}`);
    }
    if (typeof metadata.license !== "string" || metadata.license !== locked.license) {
      throw new Error(`Runtime license metadata is missing or inconsistent: ${name}`);
    }
    if (!files.some(({ name: file }) => licenseName.test(file))) {
      throw new Error(`No full runtime license text found for ${name}; review its license files.`);
    }
    for (const file of files) {
      if (!file.text.trim()) throw new Error(`Empty runtime notice: ${name}/${file.name}`);
    }
    return { name, version: metadata.version, license: metadata.license, location, files };
  });
}

/** @param {string} root */
export function buildFrontendNotices(root) {
  /** @param {string} relative */
  const readText = (relative) => normalizeNewlines(readFileSync(join(root, relative), "utf8"));
  const lockfile = JSON.parse(readText("package-lock.json"));
  const packages = collectNpmNotices(lockfile, (location) => ({
    metadata: JSON.parse(readText(`${location}/package.json`)),
    files: readdirSync(join(root, location), { withFileTypes: true })
      .filter((entry) => entry.isFile() && noticeName.test(entry.name))
      .map((entry) => entry.name)
      .sort()
      .map((name) => ({ name, text: readText(`${location}/${name}`) })),
  }));
  const sections = [
    "SSA HDRify — Third-party notices\n\n" +
      "This inventory covers the locked JavaScript runtime packages, Vite's generated " +
      "runtime helpers, and the bundled fonts and icon attribution. Some included package " +
      "notices also describe code used only during development.\n" +
      "Native Rust dependency attribution is listed separately in the project README; " +
      "this is not a complete native dependency license inventory.\n",
    `Application license\n\n${readText("LICENSE")}`,
  ];
  for (const entry of packages) {
    sections.push(
      `${entry.name} ${entry.version}\nLicense: ${entry.license}\n` +
        `Source: https://www.npmjs.com/package/${entry.name}/v/${entry.version}\n` +
        "\n" +
        entry.files.map((file) => `${file.name}\n\n${file.text}`).join("\n\n")
    );
  }
  for (const [name, source, file] of [
    ["Inter", "https://github.com/rsms/inter", "src/assets/fonts/inter/LICENSE.txt"],
    [
      "Smiley Sans",
      "https://github.com/atelier-anchor/smiley-sans",
      "src/assets/fonts/smiley-sans/LICENSE.txt",
    ],
    [
      "Feather icon attribution",
      "https://github.com/feathericons/feather/tree/v4.29.2",
      "src/assets/licenses/feather-LICENSE.txt",
    ],
  ]) {
    sections.push(`${name}\nSource: ${source}\n\n${readText(file)}`);
  }
  return sections.join("\n\n" + "=".repeat(72) + "\n\n") + "\n";
}

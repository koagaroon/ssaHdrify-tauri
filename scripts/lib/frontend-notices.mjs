import { readFileSync, readdirSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, relative, sep } from "node:path";
import { buildNativeNotices } from "./native-notices.mjs";

/**
 * @typedef {{name?: string, version?: string, license?: string, dev?: boolean, dependencies?: Record<string, string>}} PackageMetadata
 * @typedef {{lockfileVersion: number, packages: Record<string, PackageMetadata>}} NoticeLockfile
 * @typedef {{name: string, text: string}} NoticeFile
 * @typedef {{metadata: PackageMetadata, files: NoticeFile[]}} InstalledPackage
 */

/** @param {string} text */
const normalizeNewlines = (text) => text.replace(/\r\n/gu, "\n");
const licenseName = /^(?:licen[cs]e|copying)(?:$|[._-])/iu;
const noticeName = /^(?:(?:licen[cs]e|copying|notice)(?:$|[._-])|THIRD-PARTY-LICENSE$)/iu;

/** @param {NoticeLockfile} lockfile */
function resolveRolldownLocation(lockfile) {
  const vite = lockfile.packages["node_modules/vite"];
  if (!vite) throw new Error("Vite runtime helper notices are missing from the lock.");
  const requirement = /^~(\d+)\.(\d+)\.(\d+)$/u.exec(vite.dependencies?.rolldown ?? "");
  if (!requirement) {
    throw new Error(
      "Review Vite's changed Rolldown dependency requirement before building notices."
    );
  }
  const location = ["node_modules/vite/node_modules/rolldown", "node_modules/rolldown"].find(
    (candidate) => Object.hasOwn(lockfile.packages, candidate)
  );
  if (!location) throw new Error("Rolldown runtime helper notices are missing from the lock.");
  const version = /^(\d+)\.(\d+)\.(\d+)$/u.exec(lockfile.packages[location].version ?? "");
  if (
    !version ||
    version[1] !== requirement[1] ||
    version[2] !== requirement[2] ||
    Number(version[3]) < Number(requirement[3])
  ) {
    throw new Error("Locked Rolldown does not satisfy Vite's reviewed dependency requirement.");
  }
  return location;
}

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
  const rolldownLocation = resolveRolldownLocation(lockfile);
  if (!lockfile.packages["node_modules/esbuild"]) {
    throw new Error("esbuild runtime helper notices are missing from the lock.");
  }
  const locations = Object.keys(lockfile.packages)
    .filter(
      (location) =>
        location !== "" &&
        // These build tools emit helper code into the frontend or CLI engine.
        (!lockfile.packages[location].dev ||
          location === "node_modules/vite" ||
          location === "node_modules/esbuild" ||
          location === rolldownLocation)
    )
    .sort();
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
    if (
      location === "node_modules/vite" &&
      metadata.dependencies?.rolldown !== locked.dependencies?.rolldown
    ) {
      throw new Error("Installed Vite's Rolldown dependency differs from the lock.");
    }
    if (typeof metadata.license !== "string" || metadata.license !== locked.license) {
      throw new Error(`Runtime license metadata is missing or inconsistent: ${name}`);
    }
    if (!files.some(({ name: file }) => licenseName.test(file))) {
      throw new Error(`No full runtime license text found for ${name}; review its license files.`);
    }
    if (location === rolldownLocation) {
      for (const required of ["LICENSE", "THIRD-PARTY-LICENSE"]) {
        if (!files.some(({ name: file }) => file === required)) {
          throw new Error(`Missing Rolldown runtime notice: ${required}`);
        }
      }
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
  const viteRequire = createRequire(join(root, "node_modules/vite/package.json"));
  const installedRolldown = relative(root, dirname(viteRequire.resolve("rolldown/package.json")))
    .split(sep)
    .join("/");
  if (installedRolldown !== resolveRolldownLocation(lockfile)) {
    throw new Error("Installed Vite resolves a different Rolldown location than the lock.");
  }
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
      "This inventory covers the locked JavaScript runtime packages, generated runtime helpers " +
      "from Vite, Rolldown and esbuild, bundled fonts and icon attribution, the bundled SQLite source, " +
      "and unicode-normalization's code and Unicode data, followed by the locked " +
      "Windows x64 native dependency notices. The shared inventory includes " +
      "components used by only one executable and some build tools.\n",
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
    [
      "unicode-normalization Rust code (MIT option)",
      "https://crates.io/crates/unicode-normalization/0.1.26",
      "src/assets/licenses/unicode-normalization-LICENSE-MIT.txt",
    ],
    [
      "unicode-normalization Unicode data (Unicode-3.0)",
      "https://crates.io/crates/unicode-normalization/0.1.26",
      "src/assets/licenses/unicode-normalization-LICENSE-UNICODE.txt",
    ],
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
  sections.push(
    "SQLite\nSource: https://sqlite.org/\n\n" +
      "SQLite's deliverable source is in the public domain.\n" +
      "See https://sqlite.org/copyright.html.\n"
  );
  sections.push(buildNativeNotices(root));
  return sections.join("\n\n" + "=".repeat(72) + "\n\n") + "\n";
}

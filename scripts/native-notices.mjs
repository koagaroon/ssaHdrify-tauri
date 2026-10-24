import { execFileSync } from "node:child_process";
import {
  existsSync,
  lstatSync,
  readFileSync,
  readdirSync,
  realpathSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { decodeHTML } from "entities";
import {
  nativeInputs,
  isNativeLicenseFile,
  nativeManifestPath,
  nativePackages,
  nativeSourcesPath,
  nativeTarget,
  nativeTextPath,
  normalizeText,
  noticeSeparator,
  textDigest,
} from "./lib/native-notices.mjs";

/** @param {string} file */
function readText(file) {
  const stat = lstatSync(file);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 16 * 1024 * 1024) {
    throw new Error(`Notice source is not a bounded regular file: ${file}`);
  }
  const text = normalizeText(readFileSync(file, "utf8"));
  if (!text.trim()) throw new Error(`Empty notice source: ${file}`);
  return text;
}

/** @param {string} directory @param {string[]} [files] */
function licenseFiles(directory, files = []) {
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isSymbolicLink()) throw new Error(`Review linked crate source: ${path}`);
    if (entry.isDirectory()) licenseFiles(path, files);
    else if (entry.isFile() && isNativeLicenseFile(entry.name)) files.push(path);
  }
  return files.sort();
}

/** @param {string} base @param {string} path */
function containedPath(base, path) {
  if (typeof path !== "string" || !path || isAbsolute(path))
    throw new Error("Invalid notice path.");
  const full = resolve(base, path);
  const child = relative(base, full);
  if (
    !child ||
    child === ".." ||
    child.startsWith(`..${process.platform === "win32" ? "\\" : "/"}`) ||
    isAbsolute(child)
  ) {
    throw new Error("Notice path leaves its source directory.");
  }
  const physical = relative(realpathSync(base), realpathSync(full));
  if (
    physical === ".." ||
    physical.startsWith("../") ||
    physical.startsWith("..\\") ||
    isAbsolute(physical)
  ) {
    throw new Error("Notice source link leaves its source directory.");
  }
  return full;
}

/** @typedef {{path: string, url: string, sha256: string, format?: string}} SupplementFile */
/** @param {string} root @param {SupplementFile[]} files */
function readSupplements(root, files) {
  if (!Array.isArray(files) || files.length === 0)
    throw new Error("Supplemental notices are missing.");
  return files.map((file) => {
    if (
      !file ||
      !/^https:\/\//u.test(file.url) ||
      !/^[a-f0-9]{64}$/u.test(file.sha256) ||
      !file.path.startsWith("src/assets/licenses/native-sources/")
    )
      throw new Error("Supplemental notice provenance is invalid.");
    const prefix = "src/assets/licenses/native-sources/";
    const sourceText = readText(containedPath(join(root, prefix), file.path.slice(prefix.length)));
    if (textDigest(sourceText) !== file.sha256)
      throw new Error(`Supplemental notice changed: ${file.path}`);
    let text = sourceText;
    if (file.format === "rust-library-html") {
      // The Rust distribution's library copyright page contains complete license texts.
      // Preserve its text content; never render third-party HTML in the application.
      if (
        !text.includes("Copyright notices for The Rust Standard Library") ||
        /<(?:script|style)\b/iu.test(text)
      ) {
        throw new Error("Rust library copyright page format needs review.");
      }
      text = decodeHTML(
        text
          .replace(/<\/(?:h[1-6]|p|li|pre|div|section|tr)>|<br\s*\/?\s*>/giu, "\n")
          .replace(/<[^>]*>/gu, "")
      );
    } else if (file.format !== undefined) throw new Error("Unknown supplemental notice format.");
    return { name: basename(file.path), source: file.url, sha256: file.sha256, text };
  });
}

/** @param {string} root @param {string} command @param {string[]} args */
function run(root, command, args) {
  return execFileSync(command, args, {
    cwd: root,
    encoding: "utf8",
    windowsHide: true,
    timeout: 180_000,
    maxBuffer: 32 * 1024 * 1024,
    env: { ...process.env, RUSTUP_AUTO_INSTALL: "0" },
    stdio: ["ignore", "pipe", "inherit"],
  });
}

/** @param {string} root @param {string} mode @param {(command: string, args: string[]) => string} [execute] */
export function generateNativeNotices(
  root,
  mode,
  execute = (command, args) => run(root, command, args)
) {
  if (!["--check", "--write"].includes(mode)) {
    throw new Error(
      "Usage: node scripts/native-notices.mjs --check | --write. --write replaces the two tracked native notice outputs; Cargo may download locked source packages."
    );
  }
  console.log(
    `Native notices: ${mode === "--write" ? "regenerating tracked outputs" : "checking tracked outputs"}; Cargo may download locked sources.`
  );
  const inputsBefore = nativeInputs(root);
  const rustPin = readText(join(root, "rust-toolchain.toml")).match(
    /^channel\s*=\s*"(\d+\.\d+\.\d+)"/mu
  )?.[1];
  if (
    !rustPin ||
    !execute("rustc", ["--version", "--verbose"]).split(/\r?\n/u).includes(`release: ${rustPin}`)
  ) {
    throw new Error("Select the repository-pinned Rust toolchain before generating notices.");
  }
  /** @type {import("./lib/native-notices.mjs").CargoMetadata} */
  const metadata = JSON.parse(
    execute("cargo", [
      "metadata",
      "--manifest-path",
      "src-tauri/Cargo.toml",
      "--locked",
      "--filter-platform",
      nativeTarget,
      "--format-version",
      "1",
    ])
  );
  const packages = nativePackages(metadata);
  /** @type {{schemaVersion: number, target: string, packages: Record<string, {license: string, revision: string, note?: string, files: SupplementFile[]}>, supplements: {name: string, note?: string, package?: string, rust?: string, files: SupplementFile[]}[]}} */
  const sources = JSON.parse(readText(join(root, nativeSourcesPath)));
  if (
    sources.schemaVersion !== 1 ||
    sources.target !== nativeTarget ||
    !sources.packages ||
    !Array.isArray(sources.supplements)
  ) {
    throw new Error("Native supplemental source manifest needs review.");
  }
  const selected = new Set(packages.map((pkg) => `${pkg.name}@${pkg.version}`));
  for (const key of Object.keys(sources.packages)) {
    if (!selected.has(key)) throw new Error(`Stale supplemental package: ${key}`);
  }
  const sections = [
    `Native dependency notices — ${nativeTarget}\n\n` +
      "This shared inventory covers the locked Windows x64 dependency source graph for both executables. " +
      "It conservatively includes build dependencies, generated-code tools, and components used by only one executable; it is not a list of symbols linked into each file. " +
      "Other operating-system builds need a separate native notice review.\n\n" +
      "Each crates.io source link identifies the exact, unmodified upstream source package. MPL-2.0 covered source remains available there under MPL-2.0; the local SQLite override is available in this project's source repository. " +
      "License alternatives and additional notices are reproduced as supplied by their authors.\n",
  ];
  const inventory = packages.map((pkg) => {
    const key = `${pkg.name}@${pkg.version}`;
    const directory = dirname(pkg.manifest_path);
    if (
      pkg.source !== "registry+https://github.com/rust-lang/crates.io-index" &&
      !(
        pkg.source === null &&
        pkg.name === "libsqlite3-sys" &&
        resolve(directory) === resolve(root, "src-tauri/vendor/libsqlite3-sys")
      )
    ) {
      throw new Error(`Review new native dependency source: ${key}`);
    }
    const paths = licenseFiles(directory);
    if (pkg.license_file) {
      const declared = containedPath(directory, pkg.license_file);
      if (!paths.includes(declared)) paths.push(declared);
    }
    const files = paths.sort().map((file) => {
      const text = readText(file);
      return {
        name: relative(directory, file).replace(/\\/gu, "/"),
        source: "crate",
        sha256: textDigest(text),
        text,
      };
    });
    const supplement = sources.packages[key];
    if (supplement) {
      const vcs = JSON.parse(readText(join(directory, ".cargo_vcs_info.json")));
      if (supplement.license !== pkg.license || supplement.revision !== vcs.git?.sha1) {
        throw new Error(`Supplemental license does not match the locked crate: ${key}`);
      }
      files.push(...readSupplements(root, supplement.files));
    }
    if (
      files.length === 0 ||
      (!supplement &&
        !pkg.license_file &&
        !paths.some(
          (path) =>
            dirname(path) === directory ||
            relative(directory, path).replace(/\\/gu, "/").startsWith("licenses/")
        ))
    ) {
      throw new Error(
        `No primary native license text: ${key}; review its declared license and exact source provenance.`
      );
    }
    const source =
      pkg.source === null
        ? "https://github.com/koagaroon/ssaHdrify-tauri/tree/master/src-tauri/vendor/libsqlite3-sys"
        : `https://crates.io/crates/${pkg.name}/${pkg.version}`;
    sections.push(
      `${pkg.name} ${pkg.version}\nLicense: ${pkg.license}\nSource: ${source}\n` +
        (supplement?.note ? `Provenance note: ${supplement.note}\n` : "") +
        "\n" +
        files
          .map(
            (file) =>
              `${file.name}${file.source === "crate" ? "" : `\nLicense source: ${file.source}`}\n\n${file.text}`
          )
          .join("\n\n")
    );
    return {
      name: pkg.name,
      version: pkg.version,
      license: pkg.license,
      source,
      features: pkg.features,
      files: files.map(({ name, source, sha256 }) => ({ name, source, sha256 })),
    };
  });
  for (const item of sources.supplements) {
    if (
      (!item.package && !item.rust) ||
      (item.package && !selected.has(item.package)) ||
      (item.rust && item.rust !== rustPin)
    ) {
      throw new Error(`Native supplemental component changed: ${item.name}`);
    }
    const files = readSupplements(root, item.files);
    sections.push(
      `${item.name}\n` +
        (item.note ? `${item.note}\n` : "") +
        "\n" +
        files.map((file) => `${file.name}\nSource: ${file.source}\n\n${file.text}`).join("\n\n")
    );
  }
  const text = sections.join(noticeSeparator) + "\n";
  const inputsAfter = nativeInputs(root);
  if (JSON.stringify(inputsBefore) !== JSON.stringify(inputsAfter)) {
    throw new Error(
      "Native notice inputs changed during generation; retry from a stable checkout."
    );
  }
  const manifest =
    JSON.stringify(
      {
        schemaVersion: 1,
        target: nativeTarget,
        inputs: inputsBefore,
        sha256: textDigest(text),
        packages: inventory,
      },
      null,
      2
    ) + "\n";
  for (const [path, content] of [
    [nativeTextPath, text],
    [nativeManifestPath, manifest],
  ]) {
    if (mode === "--write") writeFileSync(join(root, path), content, "utf8");
    else if (
      !existsSync(join(root, path)) ||
      normalizeText(readFileSync(join(root, path), "utf8")) !== content
    ) {
      throw new Error(
        `Native notice output is stale: ${path}. Review sources, then regenerate with --write.`
      );
    }
  }
  console.log(
    `Native notices verified: ${inventory.length} crates; ${sources.supplements.length} supplemental components; ${Buffer.byteLength(text)} bytes.`
  );
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    if (process.argv.length !== 3) throw new Error("Use --check or --write as the only argument.");
    generateNativeNotices(fileURLToPath(new URL("../", import.meta.url)), process.argv[2]);
  } catch (error) {
    console.error(error instanceof Error ? error.message : "Native notice generation failed.");
    process.exitCode = 1;
  }
}

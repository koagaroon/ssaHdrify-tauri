import { createHash } from "node:crypto";
import { posix } from "node:path";
import { isDeepStrictEqual } from "node:util";
import ts from "typescript";
import { assertNpmInstallManifest, npmInstallLifecycleScripts } from "../check-npm-install.mjs";

/**
 * @typedef {"dependencies" | "devDependencies" | "optionalDependencies" | "peerDependencies"} DependencyField
 * @typedef {boolean | { name: string, version: string, isSemVerMajor: boolean }} AuditFix
 * @typedef {{ source: number, name: string, dependency: string, title: string, url: string, range: string, severity: string }} AuditAdvisory
 * @typedef {{ name: string, severity: string, isDirect: boolean, range: string, via: (string | AuditAdvisory)[], nodes: string[], effects: string[], fixAvailable: AuditFix }} AuditVulnerability
 * @typedef {{ version: string, integrity: string, inbound: string[], via: string[], effects: string[], fixAvailable: AuditFix }} ExceptionPackage
 * @typedef {{ schemaVersion: number, advisory: string, reviewedAt: string, expiresAt: string, reason: string, packages: Record<string, ExceptionPackage>, devRoots: Record<string, string>, scripts: Record<string, string>, boundaryFiles: Record<string, string> }} AuditException
 */

const levels = ["info", "low", "moderate", "high", "critical"];
/** @type {DependencyField[]} */
const dependencyFields = [
  "dependencies",
  "devDependencies",
  "optionalDependencies",
  "peerDependencies",
];

/** @param {unknown} condition @param {string} message @returns {asserts condition} */
function requirePolicy(condition, message) {
  if (!condition) throw new Error(`npm audit policy: ${message}`);
}

/** @param {unknown} value @returns {value is Record<string, unknown>} */
export function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

/** @param {unknown} value @returns {value is string[]} */
function isStringArray(value) {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

/** @param {unknown} value @returns {value is Record<string, string>} */
function isStringRecord(value) {
  return isObject(value) && Object.values(value).every((item) => typeof item === "string");
}

/** @param {unknown} value @returns {Record<string, string>} */
function dependencyMap(value) {
  requirePolicy(value === undefined || isStringRecord(value), "malformed dependency map");
  return value ?? {};
}

/** @param {unknown} actual @param {unknown} expected */
function sameMembers(actual, expected) {
  return (
    isStringArray(actual) &&
    isStringArray(expected) &&
    new Set(actual).size === actual.length &&
    isDeepStrictEqual([...actual].sort(), [...expected].sort())
  );
}

/** @param {string} text */
export function boundaryDigest(text) {
  return createHash("sha256").update(text.replace(/\r\n/gu, "\n")).digest("hex");
}

/** @param {string} name */
export function isReviewedScript(name) {
  return (
    npmInstallLifecycleScripts.includes(name) ||
    /^(?:pre|post)?(?:build(?::.*)?|lint(?::.*)?|typecheck(?::.*)?|dev|preview|tauri)$/u.test(name)
  );
}

/** @param {unknown} value @returns {value is AuditAdvisory} */
function isAdvisory(value) {
  return (
    isObject(value) &&
    typeof value.source === "number" &&
    Number.isSafeInteger(value.source) &&
    typeof value.name === "string" &&
    typeof value.dependency === "string" &&
    typeof value.title === "string" &&
    typeof value.url === "string" &&
    typeof value.range === "string" &&
    typeof value.severity === "string" &&
    levels.includes(value.severity)
  );
}

/** @param {unknown} value @returns {value is AuditFix} */
function isAuditFix(value) {
  return (
    typeof value === "boolean" ||
    (isObject(value) &&
      typeof value.name === "string" &&
      typeof value.version === "string" &&
      typeof value.isSemVerMajor === "boolean")
  );
}

/** @param {unknown} value @returns {value is AuditVulnerability} */
function isVulnerability(value) {
  return (
    isObject(value) &&
    typeof value.name === "string" &&
    typeof value.severity === "string" &&
    levels.includes(value.severity) &&
    typeof value.isDirect === "boolean" &&
    typeof value.range === "string" &&
    Array.isArray(value.via) &&
    value.via.length > 0 &&
    value.via.every((via) => typeof via === "string" || isAdvisory(via)) &&
    isStringArray(value.nodes) &&
    value.nodes.length > 0 &&
    sameMembers(value.nodes, value.nodes) &&
    sameMembers(value.effects, value.effects) &&
    isAuditFix(value.fixAvailable)
  );
}

/** @param {unknown} stdout @param {unknown} exitCode */
export function readAuditReport(stdout, exitCode) {
  requirePolicy(typeof stdout === "string", "missing audit output");
  /** @type {unknown} */
  let report;
  try {
    report = JSON.parse(stdout);
  } catch {
    throw new Error("npm audit policy: audit output is not valid JSON");
  }
  requirePolicy(
    isObject(report) &&
      !Object.hasOwn(report, "error") &&
      report.auditReportVersion === 2 &&
      isObject(report.vulnerabilities) &&
      isObject(report.metadata) &&
      isObject(report.metadata.vulnerabilities) &&
      isObject(report.metadata.dependencies),
    "unsupported or failed audit report"
  );
  const counts = Object.fromEntries(levels.map((level) => [level, 0]));
  /** @type {Record<string, AuditVulnerability>} */
  const vulnerabilities = {};
  for (const [name, item] of Object.entries(report.vulnerabilities)) {
    requirePolicy(isVulnerability(item) && item.name === name, "malformed vulnerability entry");
    for (const via of item.via) {
      if (typeof via === "string") {
        requirePolicy(Object.hasOwn(report.vulnerabilities, via), "unresolved advisory chain");
      }
    }
    vulnerabilities[name] = item;
    counts[item.severity]++;
  }
  const total = Object.keys(report.vulnerabilities).length;
  const metadata = report.metadata;
  const vulnerabilityCounts = metadata.vulnerabilities;
  const dependencyCounts = metadata.dependencies;
  requirePolicy(
    isObject(vulnerabilityCounts) && isObject(dependencyCounts),
    "missing audit counts"
  );
  requirePolicy(
    vulnerabilityCounts.total === total &&
      levels.every((level) => vulnerabilityCounts[level] === counts[level]),
    "audit counts do not match the vulnerability entries"
  );
  requirePolicy(
    ["prod", "dev", "optional", "peer", "peerOptional", "total"].every(
      (field) =>
        typeof dependencyCounts[field] === "number" &&
        Number.isSafeInteger(dependencyCounts[field]) &&
        dependencyCounts[field] >= 0
    ),
    "malformed dependency counts"
  );
  requirePolicy(exitCode === (total === 0 ? 0 : 1), "audit exit code contradicts its report");
  return { vulnerabilities };
}

/** @param {unknown} sources @param {string[]} names */
function checkSourceBoundary(sources, names) {
  requirePolicy(
    isStringRecord(sources) && Object.keys(sources).length > 0,
    "missing source inspection"
  );
  /** @param {string} value */
  const affectedReference = (value) =>
    names.some(
      (name) =>
        value.split(/[?#]/u)[0] === name ||
        value.startsWith(`${name}/`) ||
        value.replace(/\\/gu, "/").includes(`/node_modules/${name}/`) ||
        value.replace(/\\/gu, "/").endsWith(`/node_modules/${name}`)
    );
  for (const [path, text] of Object.entries(sources)) {
    requirePolicy(typeof text === "string", "invalid source inspection");
    /** @param {string} specifier */
    function checkLocalImport(specifier) {
      const target = specifier.replace(/\\/gu, "/").split(/[?#]/u)[0];
      requirePolicy(
        !target.startsWith("file:"),
        `file URL import requires boundary review in ${path}`
      );
      if (!target.startsWith(".") && !target.startsWith("/")) return;
      const resolved = posix.normalize(
        target.startsWith("/") ? target.slice(1) : posix.join(posix.dirname(path), target)
      );
      // The existing project license import is text, not an executable module.
      if (resolved === "LICENSE" && specifier.endsWith("?raw")) return;
      requirePolicy(
        resolved.startsWith("src/") || resolved.startsWith("public/"),
        `source import escapes the reviewed src/public boundary in ${path}`
      );
    }
    for (const imported of ts.preProcessFile(text, true, true).importedFiles) {
      checkLocalImport(imported.fileName);
    }
    const source = ts.createSourceFile(path, text, ts.ScriptTarget.Latest, true);
    /** @param {import("typescript").Node} node */
    function visit(node) {
      if (ts.isStringLiteralLike(node)) {
        requirePolicy(!affectedReference(node.text), `affected package referenced in ${path}`);
      }
      if (
        ts.isNewExpression(node) &&
        ts.isIdentifier(node.expression) &&
        node.expression.text === "URL" &&
        node.arguments?.[0] &&
        ts.isStringLiteralLike(node.arguments[0]) &&
        /\.[cm]?[jt]sx?(?:[?#].*)?$/u.test(node.arguments[0].text)
      ) {
        checkLocalImport(node.arguments[0].text);
      }
      if (
        ts.isCallExpression(node) &&
        (node.expression.kind === ts.SyntaxKind.ImportKeyword ||
          (ts.isIdentifier(node.expression) && node.expression.text === "require"))
      ) {
        requirePolicy(
          node.arguments.length > 0 && ts.isStringLiteralLike(node.arguments[0]),
          `computed module loading requires boundary review in ${path}`
        );
      }
      ts.forEachChild(node, visit);
    }
    visit(source);
  }
}

/** @param {unknown} value @returns {AuditException} */
export function readAuditException(value) {
  requirePolicy(
    isObject(value) &&
      value.schemaVersion === 1 &&
      value.advisory === "https://github.com/advisories/GHSA-vfj7-8cjw-p6xm" &&
      isObject(value.packages) &&
      Object.keys(value.packages).length === 7 &&
      isStringRecord(value.devRoots) &&
      isStringRecord(value.scripts) &&
      isStringRecord(value.boundaryFiles) &&
      typeof value.reviewedAt === "string" &&
      typeof value.expiresAt === "string" &&
      typeof value.reason === "string" &&
      value.reason.length > 0,
    "invalid exception configuration"
  );
  /** @type {Record<string, ExceptionPackage>} */
  const packages = {};
  for (const [name, pkg] of Object.entries(value.packages)) {
    requirePolicy(
      isObject(pkg) &&
        typeof pkg.version === "string" &&
        typeof pkg.integrity === "string" &&
        isStringArray(pkg.inbound) &&
        isStringArray(pkg.via) &&
        isStringArray(pkg.effects) &&
        isAuditFix(pkg.fixAvailable),
      "invalid exception package"
    );
    packages[name] = {
      version: pkg.version,
      integrity: pkg.integrity,
      inbound: pkg.inbound,
      via: pkg.via,
      effects: pkg.effects,
      fixAvailable: pkg.fixAvailable,
    };
  }
  return {
    schemaVersion: value.schemaVersion,
    advisory: value.advisory,
    reviewedAt: value.reviewedAt,
    expiresAt: value.expiresAt,
    reason: value.reason,
    packages,
    devRoots: value.devRoots,
    scripts: value.scripts,
    boundaryFiles: value.boundaryFiles,
  };
}

/**
 * @param {{ stdout: unknown, exitCode: unknown, manifest: unknown, lockfile: unknown, exception: unknown, boundaryFiles: unknown, sources: unknown, now?: Date }} input
 */
export function evaluateNpmAudit({
  stdout,
  exitCode,
  manifest,
  lockfile,
  exception: exceptionInput,
  boundaryFiles,
  sources,
  now = new Date(),
}) {
  const report = readAuditReport(stdout, exitCode);
  const vulnerabilities = report.vulnerabilities;
  if (Object.keys(vulnerabilities).length === 0) return { accepted: 0, warning: null };

  const exception = readAuditException(exceptionInput);
  const reviewed = Date.parse(exception.reviewedAt);
  const expires = Date.parse(exception.expiresAt);
  const current = now.getTime();
  requirePolicy(
    Number.isFinite(reviewed) &&
      Number.isFinite(expires) &&
      Number.isFinite(current) &&
      expires > reviewed &&
      expires - reviewed <= 30 * 24 * 60 * 60 * 1000 &&
      current >= reviewed,
    "invalid exception review period"
  );
  requirePolicy(current < expires, "exception expired; review the advisory before continuing");
  const names = Object.keys(exception.packages);
  requirePolicy(sameMembers(Object.keys(vulnerabilities), names), "affected package set changed");
  requirePolicy(
    isObject(manifest) &&
      isObject(lockfile) &&
      isObject(lockfile.packages) &&
      lockfile.lockfileVersion === 3,
    "missing manifest or supported lockfile"
  );
  assertNpmInstallManifest(manifest);
  const root = lockfile.packages[""];
  requirePolicy(isObject(root), "missing lockfile root");
  for (const field of dependencyFields) {
    const manifestDependencies = dependencyMap(manifest[field]);
    const rootDependencies = dependencyMap(root[field]);
    requirePolicy(
      isDeepStrictEqual(manifestDependencies, rootDependencies),
      "manifest and lockfile root dependencies differ"
    );
    for (const name of names) {
      requirePolicy(
        (manifestDependencies[name] ?? null) ===
          (field === "devDependencies" ? (exception.devRoots[name] ?? null) : null),
        "affected package changed its reviewed development-only roots"
      );
    }
  }

  /** @type {Record<string, string[]>} */
  const inbound = Object.fromEntries(names.map((name) => [name, []]));
  for (const [path, pkg] of Object.entries(lockfile.packages)) {
    requirePolicy(isObject(pkg), "malformed lockfile package");
    requirePolicy(pkg.name === undefined || typeof pkg.name === "string", "malformed package name");
    const packageName = pkg.name ?? path.split("node_modules/").at(-1) ?? "";
    if (names.includes(packageName)) {
      requirePolicy(path === `node_modules/${packageName}`, "additional affected lockfile node");
    }
    for (const field of dependencyFields) {
      for (const [name, range] of Object.entries(dependencyMap(pkg[field]))) {
        if (names.includes(name)) inbound[name].push(`${path}:${field}:${range}`);
      }
    }
  }
  for (const name of names) {
    const expected = exception.packages[name];
    const item = vulnerabilities[name];
    const path = `node_modules/${name}`;
    const pkg = lockfile.packages[path];
    requirePolicy(
      isObject(pkg) &&
        pkg.dev === true &&
        !pkg.link &&
        pkg.version === expected.version &&
        pkg.integrity === expected.integrity &&
        pkg.resolved === `https://registry.npmjs.org/${name}/-/${name}-${expected.version}.tgz`,
      `reviewed development-only lock identity changed for ${name}`
    );
    requirePolicy(
      sameMembers(inbound[name], expected.inbound),
      `dependency edges changed for ${name}`
    );
    requirePolicy(
      isDeepStrictEqual(item.fixAvailable, expected.fixAvailable),
      `npm's suggested fix changed for ${name}; review the available remediation`
    );
    requirePolicy(
      item.severity === "high" &&
        item.isDirect === Object.hasOwn(exception.devRoots, name) &&
        sameMembers(item.nodes, [path]) &&
        sameMembers(item.effects, expected.effects) &&
        sameMembers(
          item.via.map((via) => (typeof via === "string" ? via : via.url)),
          expected.via
        ),
      `advisory graph changed for ${name}`
    );
    for (const via of item.via.filter((value) => typeof value !== "string")) {
      requirePolicy(
        name === "braces" &&
          via.name === name &&
          via.dependency === name &&
          via.url === exception.advisory &&
          via.severity === "high" &&
          via.range === "<=3.0.3",
        "unreviewed advisory"
      );
    }
  }

  const relevantScripts = Object.fromEntries(
    Object.entries(dependencyMap(manifest.scripts)).filter(([name]) => isReviewedScript(name))
  );
  requirePolicy(
    !Object.hasOwn(manifest, "stylelint") && isDeepStrictEqual(relevantScripts, exception.scripts),
    "lint or build scripts changed; review the exception boundary"
  );
  requirePolicy(
    isStringRecord(boundaryFiles) &&
      sameMembers(Object.keys(boundaryFiles), Object.keys(exception.boundaryFiles)) &&
      Object.entries(exception.boundaryFiles).every(
        ([path, digest]) =>
          typeof boundaryFiles[path] === "string" && boundaryDigest(boundaryFiles[path]) === digest
      ),
    "lint or bundle configuration changed; review the exception boundary"
  );
  checkSourceBoundary(sources, names);
  return {
    accepted: names.length,
    warning: `Reviewed exception: ${exception.advisory}\nReason: ${exception.reason}\nExpires: ${exception.expiresAt}\n${names.length} affected development-only package entries remain visible; all other advisories fail.`,
  };
}

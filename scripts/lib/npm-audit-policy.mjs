import { isDeepStrictEqual } from "node:util";

/**
 * @typedef {boolean | { name: string, version: string, isSemVerMajor: boolean }} AuditFix
 * @typedef {{ source: number, name: string, dependency: string, title: string, url: string, range: string, severity: string }} AuditAdvisory
 * @typedef {{ name: string, severity: string, isDirect: boolean, range: string, via: (string | AuditAdvisory)[], nodes: string[], effects: string[], fixAvailable: AuditFix }} AuditVulnerability
 */

const levels = ["info", "low", "moderate", "high", "critical"];

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

/** @param {unknown} actual @param {unknown} expected */
function sameMembers(actual, expected) {
  return (
    isStringArray(actual) &&
    isStringArray(expected) &&
    new Set(actual).size === actual.length &&
    isDeepStrictEqual([...actual].sort(), [...expected].sort())
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
  const vulnerabilities = Object.create(null);
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

/** @param {{ stdout: unknown, exitCode: unknown }} input */
export function evaluateNpmAudit({ stdout, exitCode }) {
  const { vulnerabilities } = readAuditReport(stdout, exitCode);
  const affected = Object.entries(vulnerabilities).map(
    ([name, item]) => `${name} (${item.severity})`
  );
  requirePolicy(affected.length === 0, `vulnerabilities reported: ${affected.join(", ")}`);
}

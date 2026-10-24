import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, test } from "vitest";
import { evaluateNpmAudit, readAuditReport } from "./npm-audit-policy.mjs";

/**
 * @typedef {import("./npm-audit-policy.mjs").AuditVulnerability} AuditVulnerability
 * @typedef {{ auditReportVersion: number, vulnerabilities: Record<string, AuditVulnerability>, metadata: { vulnerabilities: Record<string, number>, dependencies?: Record<string, number> }, error?: unknown }} FixtureReport
 */

/** @returns {FixtureReport} */
function cleanReport() {
  return {
    auditReportVersion: 2,
    vulnerabilities: {},
    metadata: {
      vulnerabilities: { info: 0, low: 0, moderate: 0, high: 0, critical: 0, total: 0 },
      dependencies: { prod: 2, dev: 3, optional: 0, peer: 0, peerOptional: 0, total: 5 },
    },
  };
}

/** @param {string} severity @param {string} name @returns {FixtureReport} */
function vulnerableReport(severity = "high", name = "braces") {
  const report = cleanReport();
  report.vulnerabilities = Object.fromEntries([
    [
      name,
      {
        name,
        severity,
        isDirect: false,
        range: "<=3.0.3",
        nodes: [`node_modules/${name}`],
        effects: [],
        via: [
          {
            source: 1240992,
            name,
            dependency: name,
            title: "braces stack exhaustion",
            url: "https://github.com/advisories/GHSA-vfj7-8cjw-p6xm",
            severity,
            range: "<=3.0.3",
          },
        ],
        fixAvailable: false,
      },
    ],
  ]);
  report.metadata.vulnerabilities[severity] = 1;
  report.metadata.vulnerabilities.total = 1;
  return report;
}

/** @param {FixtureReport} report @param {unknown} exitCode */
function evaluate(report, exitCode) {
  return evaluateNpmAudit({ stdout: JSON.stringify(report), exitCode });
}

describe("the complete npm audit gate without exceptions", () => {
  test("accepts a complete clean report with a successful process status", () => {
    expect(() => evaluate(cleanReport(), 0)).not.toThrow();
  });

  test.each(["info", "low", "moderate", "high", "critical"])(
    "rejects a reported %s vulnerability",
    (severity) => {
      expect(() => evaluate(vulnerableReport(severity, "affected-package"), 1)).toThrow(
        `vulnerabilities reported: affected-package (${severity})`
      );
    }
  );

  test("rejects the formerly excepted braces advisory if it is reported again", () => {
    expect(() => evaluate(vulnerableReport(), 1)).toThrow(
      "vulnerabilities reported: braces (high)"
    );
  });

  test("does not lose a vulnerability whose name is an object prototype property", () => {
    expect(() => evaluate(vulnerableReport("high", "__proto__"), 1)).toThrow(
      "vulnerabilities reported: __proto__ (high)"
    );
  });
});

describe("npm audit report and exit validation", () => {
  test.each([undefined, "", "null", "[]", "not JSON", '{"error":{"code":"E401"}}'])(
    "rejects missing, malformed, or API error output: %s",
    (stdout) => expect(() => readAuditReport(stdout, 1)).toThrow()
  );

  test.each(
    /** @type {[string, (report: FixtureReport) => void][]} */ ([
      ["version", (r) => (r.auditReportVersion = 3)],
      ["null entries", (r) => Object.assign(r, { vulnerabilities: null })],
      ["null via", (r) => Object.assign(r.vulnerabilities.braces, { via: null })],
      ["null advisory", (r) => Object.assign(r.vulnerabilities.braces, { via: [null] })],
      ["unresolved reference", (r) => (r.vulnerabilities.braces.via = ["missing"])],
      ["mismatched name", (r) => (r.vulnerabilities.braces.name = "other")],
      ["unknown severity", (r) => (r.vulnerabilities.braces.severity = "unknown")],
      ["missing nodes", (r) => (r.vulnerabilities.braces.nodes = [])],
      ["invalid fix", (r) => Object.assign(r.vulnerabilities.braces, { fixAvailable: null })],
      ["wrong count", (r) => (r.metadata.vulnerabilities.total = 0)],
      ["wrong severity count", (r) => (r.metadata.vulnerabilities.high = 0)],
      ["missing dependency counts", (r) => delete r.metadata.dependencies],
      ["API error", (r) => (r.error = { code: "E401" })],
    ])
  )("rejects malformed report data before evaluating vulnerabilities: %s", (_label, mutate) => {
    const report = vulnerableReport();
    mutate(report);
    expect(() => readAuditReport(JSON.stringify(report), 1)).toThrow();
  });

  test.each([undefined, -1, 1.5, "0"])(
    "rejects an otherwise clean report with an invalid dependency count: %s",
    (count) => {
      const report = cleanReport();
      Object.assign(report.metadata, {
        dependencies: { ...report.metadata.dependencies, dev: count },
      });
      expect(() => evaluate(report, 0)).toThrow("malformed dependency counts");
    }
  );

  test("rejects a seemingly clean report with nonzero vulnerability counts", () => {
    const report = cleanReport();
    report.metadata.vulnerabilities.high = 1;
    report.metadata.vulnerabilities.total = 1;
    expect(() => evaluate(report, 0)).toThrow("audit counts");
  });

  test.each([0, 2, null, "1"])("rejects inconsistent vulnerable status %s", (exitCode) => {
    expect(() => evaluate(vulnerableReport(), exitCode)).toThrow("exit code");
  });

  test.each([1, 2, null, "0"])("rejects inconsistent clean status %s", (exitCode) => {
    expect(() => evaluate(cleanReport(), exitCode)).toThrow("exit code");
  });
});

/**
 * @param {{ stdout?: string, exitCode?: number, npmVersion?: string, differentBin?: boolean, missingNpmPath?: boolean, scripts?: Record<string, string> }} options
 */
function runAuditCommand({
  stdout = JSON.stringify(cleanReport()),
  exitCode = 0,
  npmVersion = "1.2.3",
  differentBin = false,
  missingNpmPath = false,
  scripts = {},
} = {}) {
  const root = mkdtempSync(join(tmpdir(), "ssahdrify-audit-policy-"));
  try {
    mkdirSync(join(root, "scripts", "lib"), { recursive: true });
    for (const name of [
      "check-npm-audit.mjs",
      "check-npm-install.mjs",
      "lib/npm-audit-policy.mjs",
    ]) {
      copyFileSync(new URL(`../${name}`, import.meta.url), join(root, "scripts", name));
    }
    writeFileSync(
      join(root, "package.json"),
      JSON.stringify({ name: "audit-fixture", packageManager: "npm@1.2.3", scripts }),
      "utf8"
    );
    const npmRoot = join(root, "fixture-npm");
    mkdirSync(join(npmRoot, "bin"), { recursive: true });
    writeFileSync(
      join(npmRoot, "package.json"),
      JSON.stringify({
        name: "npm",
        version: npmVersion,
        type: "module",
        bin: { npm: differentBin ? "bin/other.js" : "bin/npm-cli.js" },
      }),
      "utf8"
    );
    writeFileSync(join(npmRoot, "response.json"), JSON.stringify({ stdout, exitCode }), "utf8");
    const npmCli = join(npmRoot, "bin", "npm-cli.js");
    writeFileSync(
      npmCli,
      `import { deepStrictEqual } from "node:assert";
import { readFileSync } from "node:fs";
deepStrictEqual(process.argv.slice(2), [
  "audit", "--json", "--include=dev", "--include=optional", "--include=peer", "--audit-level=info"
]);
const response = JSON.parse(readFileSync(new URL("../response.json", import.meta.url), "utf8"));
process.stdout.write(response.stdout);
process.exitCode = response.exitCode;
`,
      "utf8"
    );
    if (differentBin) copyFileSync(npmCli, join(npmRoot, "bin", "other.js"));
    // Windows treats environment names case-insensitively, even when a worker exposes uppercase keys.
    const env = Object.fromEntries(
      Object.entries(process.env).filter(([name]) => name.toLowerCase() !== "npm_execpath")
    );
    env.npm_execpath = missingNpmPath ? "" : npmCli;
    return spawnSync(process.execPath, [join(root, "scripts", "check-npm-audit.mjs")], {
      cwd: root,
      env,
      encoding: "utf8",
      timeout: 10_000,
      windowsHide: true,
    });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

describe("the npm audit command", () => {
  test("audits every dependency category without an exception file or source inspection", () => {
    const result = runAuditCommand();
    expect(result.error).toBeUndefined();
    expect(result.status).toBe(0);
    expect(result.stdout).toContain("no vulnerabilities reported in the complete dependency graph");
  });

  test("fails the command for the former braces exception", () => {
    const result = runAuditCommand({ stdout: JSON.stringify(vulnerableReport()), exitCode: 1 });
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("vulnerabilities reported: braces (high)");
  });

  test("does not turn an audit process failure into a clean result", () => {
    const result = runAuditCommand({ exitCode: 2 });
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("audit exit code contradicts its report");
  });

  test("rejects npm versions that differ from packageManager", () => {
    const result = runAuditCommand({ npmVersion: "1.2.4" });
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("exact npm version declared");
  });

  test("rejects an executable that differs from npm's declared binary", () => {
    const result = runAuditCommand({ differentBin: true });
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("exact npm version declared");
  });

  test("requires the npm invocation path", () => {
    const result = runAuditCommand({ missingNpmPath: true });
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("npm run audit:dependencies");
  });

  test("still applies the installation preflight before running npm audit", () => {
    const result = runAuditCommand({ scripts: { postinstall: "node install-helper.mjs" } });
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("installation scripts require review: postinstall");
  });
});

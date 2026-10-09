import { readFile } from "node:fs/promises";
import { posix } from "node:path";
import ts from "typescript";
import { describe, expect, test } from "vitest";
import {
  boundaryDigest,
  evaluateNpmAudit,
  readAuditException,
  readAuditReport,
} from "./npm-audit-policy.mjs";

/**
 * @typedef {import("./npm-audit-policy.mjs").DependencyField} DependencyField
 * @typedef {import("./npm-audit-policy.mjs").AuditVulnerability} AuditVulnerability
 * @typedef {import("./npm-audit-policy.mjs").AuditException} AuditException
 * @typedef {Partial<Record<DependencyField, Record<string, string>>> & { scripts: Record<string, string> }} FixtureManifest
 * @typedef {Partial<Record<DependencyField, Record<string, string>>> & { name?: string, version?: string, integrity?: string, resolved?: string, dev?: boolean }} FixturePackage
 * @typedef {{ auditReportVersion: number, vulnerabilities: Record<string, AuditVulnerability>, metadata: { vulnerabilities: Record<string, number>, dependencies?: Record<string, number> }, error?: unknown }} FixtureReport
 * @typedef {{ report: FixtureReport, exitCode: unknown, manifest: FixtureManifest, lockfile: { lockfileVersion: number, packages: Record<string, FixturePackage> }, exception: AuditException, boundaryFiles: Record<string, string>, sources: Record<string, string>, now: Date }} Fixture
 */

const root = new URL("../../", import.meta.url);
const exception = readAuditException(
  JSON.parse(await readFile(new URL("scripts/npm-audit-exceptions.json", root), "utf8"))
);
const boundaryFiles = Object.fromEntries(
  await Promise.all(
    Object.keys(exception.boundaryFiles).map(async (path) => [
      path,
      await readFile(new URL(path, root), "utf8"),
    ])
  )
);

/** @returns {Fixture} */
function fixture() {
  /** @type {FixtureManifest} */
  const manifest = {
    scripts: structuredClone(exception.scripts),
    devDependencies: structuredClone(exception.devRoots),
  };
  /** @type {Fixture["lockfile"]} */
  const lockfile = {
    lockfileVersion: 3,
    packages: { "": { devDependencies: structuredClone(exception.devRoots) } },
  };
  /** @type {Record<string, AuditVulnerability>} */
  const vulnerabilities = {};
  for (const [name, pkg] of Object.entries(exception.packages)) {
    lockfile.packages[`node_modules/${name}`] = {
      version: pkg.version,
      integrity: pkg.integrity,
      resolved: `https://registry.npmjs.org/${name}/-/${name}-${pkg.version}.tgz`,
      dev: true,
    };
    vulnerabilities[name] = {
      name,
      severity: "high",
      isDirect: Object.hasOwn(exception.devRoots, name),
      range: "*",
      nodes: [`node_modules/${name}`],
      effects: structuredClone(pkg.effects),
      via: pkg.via.map((via) =>
        via === exception.advisory
          ? {
              source: 1240992,
              name,
              dependency: name,
              title: "braces stack exhaustion",
              url: via,
              severity: "high",
              range: "<=3.0.3",
            }
          : via
      ),
      fixAvailable: structuredClone(pkg.fixAvailable),
    };
  }
  for (const [name, pkg] of Object.entries(exception.packages)) {
    for (const edge of pkg.inbound) {
      const [path, fieldName, range] = edge.split(":");
      if (
        !["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"].includes(
          fieldName
        )
      ) {
        throw new Error("Invalid fixture dependency field");
      }
      const field = /** @type {DependencyField} */ (fieldName);
      lockfile.packages[path][field] ??= {};
      lockfile.packages[path][field][name] = range;
    }
  }
  return structuredClone({
    report: {
      auditReportVersion: 2,
      vulnerabilities,
      metadata: {
        vulnerabilities: { info: 0, low: 0, moderate: 0, high: 7, critical: 0, total: 7 },
        dependencies: { prod: 0, dev: 7, optional: 0, peer: 0, peerOptional: 0, total: 7 },
      },
    },
    exitCode: 1,
    manifest,
    lockfile,
    exception,
    boundaryFiles,
    sources: { "src/main.ts": 'import React from "react";' },
    now: new Date("2026-10-09T12:00:00+08:00"),
  });
}

/** @param {Fixture} input */
function evaluate(input) {
  const { report, ...options } = input;
  return evaluateNpmAudit({ stdout: JSON.stringify(report), ...options });
}

/** @param {AuditVulnerability} vulnerability */
function firstAdvisory(vulnerability) {
  const advisory = vulnerability.via[0];
  if (typeof advisory === "string") throw new Error("Fixture has no direct advisory");
  return advisory;
}

describe("the single reviewed npm audit exception", () => {
  test("includes local imports of every reviewed build helper in the boundary", () => {
    for (const [path, source] of Object.entries(boundaryFiles)) {
      if (!/\.[cm]?[jt]s$/u.test(path)) continue;
      for (const imported of ts.preProcessFile(source, true, true).importedFiles) {
        if (!imported.fileName.startsWith(".")) continue;
        const target = posix.normalize(posix.join(posix.dirname(path), imported.fileName));
        expect(Object.hasOwn(boundaryFiles, target), `${path} imports ${target}`).toBe(true);
      }
    }
  });

  test("accepts the seven reviewed packages and visibly identifies the reason and expiry", () => {
    const result = evaluate(fixture());
    expect(result.accepted).toBe(7);
    expect(result.warning).toContain(exception.advisory);
    expect(result.warning).toContain(exception.reason);
    expect(result.warning).toContain(exception.expiresAt);
  });

  test("accepts a clean report without applying an expired exception", () => {
    const input = fixture();
    input.report.vulnerabilities = {};
    input.report.metadata.vulnerabilities.high = 0;
    input.report.metadata.vulnerabilities.total = 0;
    input.exitCode = 0;
    input.now = new Date("2027-01-01T00:00:00Z");
    expect(evaluate(input)).toEqual({ accepted: 0, warning: null });
  });

  test("expires at the stated instant, after accepting the preceding millisecond", () => {
    const input = fixture();
    input.now = new Date(Date.parse(exception.expiresAt) - 1);
    expect(evaluate(input).accepted).toBe(7);
    input.now = new Date(exception.expiresAt);
    expect(() => evaluate(input)).toThrow("expired");
    input.now = new Date(Date.parse(exception.expiresAt) + 1);
    expect(() => evaluate(input)).toThrow("expired");
  });

  test("rejects an invalid clock or an exception longer than 30 days", () => {
    const input = fixture();
    input.now = new Date("invalid");
    expect(() => evaluate(input)).toThrow("review period");
    input.now = new Date("2026-10-01T00:00:00Z");
    expect(() => evaluate(input)).toThrow("review period");
    input.now = new Date("2026-10-10T00:00:00Z");
    input.exception.expiresAt = "2026-11-09T00:00:00+08:00";
    expect(() => evaluate(input)).toThrow("review period");
  });

  test("fails a new advisory even when it affects the same reviewed package", () => {
    const input = fixture();
    firstAdvisory(input.report.vulnerabilities.braces).url =
      "https://github.com/advisories/GHSA-aaaa-bbbb-cccc";
    expect(() => evaluate(input)).toThrow("advisory graph");
  });

  test.each([
    true,
    false,
    { name: "stylelint", version: "17.16.1", isSemVerMajor: false },
    { name: "stylelint", version: "7.7.0", isSemVerMajor: false },
  ])("requires review when npm's suggested fix changes to %j", (fixAvailable) => {
    const input = fixture();
    input.report.vulnerabilities.braces.fixAvailable = fixAvailable;
    expect(() => evaluate(input)).toThrow("suggested fix changed");
  });

  test.each(["extra", "missing"])("fails a %s affected package", (change) => {
    const input = fixture();
    if (change === "extra") {
      input.report.vulnerabilities.other = {
        ...input.report.vulnerabilities.braces,
        name: "other",
        nodes: ["node_modules/other"],
      };
    } else delete input.report.vulnerabilities["stylelint-config-standard"];
    const count = Object.keys(input.report.vulnerabilities).length;
    input.report.metadata.vulnerabilities.high = count;
    input.report.metadata.vulnerabilities.total = count;
    expect(() => evaluate(input)).toThrow("affected package set");
  });

  test.each(
    /** @type {[string, (values: Record<string, AuditVulnerability>) => void][]} */ ([
      [
        "another advisory",
        (v) => v.braces.via.push({ ...firstAdvisory(v.braces), url: "https://example.com/other" }),
      ],
      ["duplicate advisory", (v) => v.braces.via.push(v.braces.via[0])],
      ["different via edge", (v) => (v.globby.via = ["micromatch"])],
      ["different effect edge", (v) => (v.braces.effects = [])],
      ["extra node", (v) => v.braces.nodes.push("node_modules/other/node_modules/braces")],
      ["different node", (v) => (v.braces.nodes = ["node_modules/other/node_modules/braces"])],
    ])
  )("fails %s in the advisory graph", (_label, mutate) => {
    const input = fixture();
    mutate(input.report.vulnerabilities);
    expect(() => evaluate(input)).toThrow("advisory graph");
  });

  test.each(
    /** @type {[string, (values: Record<string, FixturePackage>) => void][]} */ ([
      ["changed version", (p) => (p["node_modules/braces"].version = "3.0.4")],
      ["changed integrity", (p) => (p["node_modules/braces"].integrity = "sha512-other")],
      ["runtime inclusion", (p) => (p["node_modules/braces"].dev = false)],
      ["missing node", (p) => delete p["node_modules/braces"]],
      [
        "additional copy",
        (p) => (p["node_modules/x/node_modules/braces"] = { version: "3.0.3", dev: true }),
      ],
      [
        "aliased copy",
        (p) => (p["node_modules/alias"] = { name: "braces", version: "3.0.3", dev: true }),
      ],
      [
        "new incoming edge",
        (p) => (p["node_modules/other"] = { dependencies: { braces: "^3.0.3" } }),
      ],
      [
        "changed incoming edge",
        (p) => {
          const dependencies = p["node_modules/micromatch"].dependencies;
          if (!dependencies) throw new Error("Missing fixture dependency map");
          dependencies.braces = "*";
        },
      ],
    ])
  )("fails %s in the lockfile", (_label, mutate) => {
    const input = fixture();
    mutate(input.lockfile.packages);
    expect(() => evaluate(input)).toThrow();
  });

  test("fails a new runtime root even when lockfile dev flags are still true", () => {
    const input = fixture();
    input.manifest.dependencies = { stylelint: "^17.16.0" };
    input.lockfile.packages[""].dependencies = input.manifest.dependencies;
    expect(() => evaluate(input)).toThrow("development-only roots");
  });

  test("fails disagreement between the manifest and lockfile", () => {
    const input = fixture();
    input.manifest.dependencies = { example: "1.0.0" };
    expect(() => evaluate(input)).toThrow("manifest and lockfile");
  });

  test("requires review when lint patterns, build scripts, or configuration change", () => {
    const input = fixture();
    input.manifest.scripts["lint:css"] = 'stylelint "{a,{b,c}}/**/*.css"';
    expect(() => evaluate(input)).toThrow("lint or build scripts");
    input.manifest.scripts = structuredClone(exception.scripts);
    input.manifest.scripts["build:other"] = "node other.mjs";
    expect(() => evaluate(input)).toThrow("lint or build scripts");
    input.manifest.scripts = structuredClone(exception.scripts);
    input.boundaryFiles["vite.config.ts"] += "\n// Changed build behavior needs review.\n";
    expect(() => evaluate(input)).toThrow("configuration changed");
  });

  test.each(["scripts/lib/app-version.mjs", "scripts/lib/frontend-notices.mjs"])(
    "requires review when a transitive build helper changes: %s",
    (path) => {
      const input = fixture();
      input.boundaryFiles[path] += '\nimport "braces/lib/parse.js";\n';
      expect(() => evaluate(input)).toThrow("configuration changed");
    }
  );

  test.each(["prelint:css", "postlint:css", "prebuild", "postbuild:engine", "pretypecheck:ts7"])(
    "rejects an unreviewed npm lifecycle hook: %s",
    (hook) => {
      const input = fixture();
      input.manifest.scripts[hook] = 'stylelint "{a,{b,c}}/**/*.css"';
      expect(() => evaluate(input)).toThrow("lint or build scripts");
    }
  );

  test("also rejects root install hooks in the full audit policy", () => {
    const input = fixture();
    input.manifest.scripts.postinstall = "node install-helper.mjs";
    expect(() => evaluate(input)).toThrow("installation scripts require review");
  });

  test.each([
    'export { default } from "../scripts/runtime-helper.mjs";',
    'const helper = import("../scripts/runtime-helper.mjs");',
    'const helper = require("../scripts/runtime-helper.mjs");',
    'const worker = new Worker(new URL("../scripts/runtime-helper.mjs", import.meta.url));',
    'export { value } from "../src-other/runtime-helper.mjs";',
    'export { value } from "./lib/../../scripts/runtime-helper.mjs";',
  ])("rejects imports that could hide braces/lib/parse.js in an unscanned helper: %s", (source) => {
    const input = fixture();
    input.sources["src/main.ts"] = source;
    expect(() => evaluate(input)).toThrow("escapes the reviewed src/public boundary");
  });

  test("keeps local source imports inside the inspected trees usable", () => {
    const input = fixture();
    input.sources["src/main.ts"] = 'export { value } from "./lib/helper.ts";';
    input.sources["src/lib/helper.ts"] = "export const value = 1;";
    expect(evaluate(input).accepted).toBe(7);
  });

  test("preserves the existing raw license import and license-file URL reads", () => {
    const input = fixture();
    input.sources = {
      "src/lib/license-notices.ts": 'import license from "../../LICENSE?raw";',
      "src/lib/license-notices.test.ts":
        'const license = new URL("../../src-tauri/vendor/libsqlite3-sys/LICENSE", import.meta.url);',
    };
    expect(evaluate(input).accepted).toBe(7);
  });

  test.each([
    'import x from "braces";',
    'export { x } from "stylelint/lib/standalone.mjs";',
    'const x = require("micromatch");',
    'const x = import("braces");',
    'const x = import("brace" + "s");',
    'const x = new URL("../node_modules/braces/index.js", import.meta.url);',
  ])("fails shipped-source references or computed module loading: %s", (source) => {
    const input = fixture();
    input.sources["src/main.ts"] = source;
    expect(() => evaluate(input)).toThrow();
  });

  test("accepts the same boundary content with Windows or Unix line endings", () => {
    const input = fixture();
    for (const path of Object.keys(input.boundaryFiles)) {
      input.boundaryFiles[path] = input.boundaryFiles[path]
        .replace(/\r\n/gu, "\n")
        .replace(/\n/gu, "\r\n");
    }
    expect(evaluate(input).accepted).toBe(7);
    expect(boundaryDigest("a\r\nb\r\n")).toBe(boundaryDigest("a\nb\n"));
  });
});

describe("npm audit report and exit validation", () => {
  test.each(["", "null", "[]", "not JSON", '{"error":{"code":"E401"}}'])(
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
      ["wrong count", (r) => (r.metadata.vulnerabilities.total = 0)],
      ["wrong severity count", (r) => (r.metadata.vulnerabilities.high = 0)],
      ["missing dependency counts", (r) => delete r.metadata.dependencies],
      ["API error", (r) => (r.error = { code: "E401" })],
    ])
  )("rejects malformed report data: %s", (_label, mutate) => {
    const input = fixture();
    mutate(input.report);
    expect(() => evaluate(input)).toThrow();
  });

  test.each([0, 2, null, "1"])("rejects inconsistent process status %s", (exitCode) => {
    const input = fixture();
    input.exitCode = exitCode;
    expect(() => evaluate(input)).toThrow("exit code");
  });
});

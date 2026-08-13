import { readFile } from "node:fs/promises";

import { describe, expect, test, vi } from "vitest";

import {
  MonitorDataError,
  classifyActionCurrency,
  classifyCargoCurrency,
  classifyNpmCurrency,
  collectCargoTargets,
  collectNpmTargets,
  cratesIndexPath,
  dependencyWatchExitCode,
  fetchTextWithPolicy,
  inspectGitHubReleases,
  inspectNpmMetadata,
  inspectResolvedCommitSha,
  maxStableVersion,
  parseActionReferences,
  parseCratesIndex,
  parseSemver,
  renderDependencyReport,
  sanitizeReportField,
} from "./dependency-currency.mjs";

const repositoryRoot = new URL("../../", import.meta.url);
const SHA_ONE = "1".repeat(40);
const SHA_TWO = "2".repeat(40);

/**
 * @param {string} name
 * @param {string[]} versions
 * @param {string} latest
 */
function npmMetadata(name, versions, latest) {
  return {
    name,
    "dist-tags": { latest },
    versions: Object.fromEntries(versions.map((version) => [version, { name, version }])),
  };
}

describe("semantic version validation", () => {
  test("orders numeric components rather than comparing text", () => {
    expect(maxStableVersion(["1.2.9", "1.10.0", "1.9.8"])).toBe("1.10.0");
  });

  test("rejects malformed and prerelease values from the stable channel", () => {
    expect(parseSemver("01.2.3")).toBeNull();
    expect(parseSemver("1.2.3\n::error::boom")).toBeNull();
    expect(maxStableVersion(["2.0.0-beta.1", "1.9.0"])).toBe("1.9.0");
  });
});

describe("npm direct dependencies", () => {
  const packageJson = {
    dependencies: { react: "^19.2.0" },
    devDependencies: {
      "@types/node": "~22.13.17",
      typescript: "npm:@typescript/typescript6@6.0.2",
    },
  };
  const lockfile = {
    packages: {
      "": {
        dependencies: { react: "^19.2.0" },
        devDependencies: {
          "@types/node": "~22.13.17",
          typescript: "npm:@typescript/typescript6@6.0.2",
        },
      },
      "node_modules/react": { version: "19.2.8" },
      "node_modules/@types/node": { version: "22.13.17" },
      "node_modules/transitive-only": { version: "99.0.0" },
    },
  };

  test("uses only direct locked packages and separates npm aliases", () => {
    const result = collectNpmTargets(packageJson, lockfile);
    expect(result.aliases).toEqual(["typescript"]);
    expect(result.targets).toEqual([
      { name: "react", requested: "^19.2.0", locked: "19.2.8", kind: "dependency" },
      {
        name: "@types/node",
        requested: "~22.13.17",
        locked: "22.13.17",
        kind: "development",
      },
    ]);
  });

  test("rejects a stale root lock declaration", () => {
    const staleLock = structuredClone(lockfile);
    staleLock.packages[""].dependencies.react = "^18.0.0";
    expect(() => collectNpmTargets(packageJson, staleLock)).toThrow(MonitorDataError);
  });

  test("reports an in-line patch while retaining the Node typings hold", () => {
    const inspection = inspectNpmMetadata(
      npmMetadata("@types/node", ["22.13.17", "22.13.18", "26.2.0"], "26.2.0"),
      "@types/node",
      { major: 22, minor: 13 }
    );
    const result = classifyNpmCurrency({
      locked: "22.13.17",
      inspection,
      linePolicy: { outsideLine: "held", reason: "Keep the Node 22.13 API floor." },
    });
    expect(result.status).toBe("update");
    expect(result.latest).toBe("22.13.18");
    expect(result.reason).toContain("26.2.0");
  });

  test("shows later held lines without keeping a current tracked line red", () => {
    const inspection = inspectNpmMetadata(
      npmMetadata("@types/node", ["22.13.18", "26.2.0"], "26.2.0"),
      "@types/node",
      { major: 22, minor: 13 }
    );
    const result = classifyNpmCurrency({
      locked: "22.13.18",
      inspection,
      linePolicy: { outsideLine: "held", reason: "Keep the Node 22.13 API floor." },
    });
    expect(result).toMatchObject({ latest: "26.2.0", status: "held" });
  });

  test("rejects an incomplete selected npm line record", () => {
    const metadata = npmMetadata("@types/node", ["22.13.17", "22.13.18", "26.2.0"], "26.2.0");
    metadata.versions["22.13.18"].version = "22.13.17";
    expect(() => inspectNpmMetadata(metadata, "@types/node", { major: 22, minor: 13 })).toThrow(
      "incomplete selected-version metadata"
    );
  });
});

describe("Cargo direct dependencies", () => {
  test("builds sparse-index paths for crates.io", () => {
    expect(cratesIndexPath("a")).toBe("1/a");
    expect(cratesIndexPath("ab")).toBe("2/ab");
    expect(cratesIndexPath("abc")).toBe("3/a/abc");
    expect(cratesIndexPath("Serde_JSON")).toBe("se/rd/serde_json");
  });

  test("parses supported index entries and ignores prerelease or yanked candidates", () => {
    const versions = parseCratesIndex(
      [
        JSON.stringify({ name: "demo", vers: "1.0.0", yanked: false, rust_version: "1.80" }),
        JSON.stringify({ name: "demo", vers: "1.1.0-beta.1", yanked: false }),
        JSON.stringify({ name: "demo", vers: "1.1.0", yanked: true }),
        JSON.stringify({ name: "demo", vers: "1.0.1", yanked: false, rust_version: null, v: 2 }),
        JSON.stringify({ name: "demo", vers: "9.0.0", yanked: false, v: 3 }),
      ].join("\n"),
      "demo"
    );
    const result = classifyCargoCurrency({
      locked: "1.0.0",
      versions,
      mode: "default",
      policyReason: "",
      msrv: "1.91",
    });
    expect(result).toMatchObject({ latest: "1.0.1", status: "update" });
    expect(result.reason).toContain("verification is required");
  });

  test("does not claim Rust compatibility from crate metadata alone", () => {
    const result = classifyCargoCurrency({
      locked: "1.0.0",
      versions: [
        { version: "1.0.0", yanked: false, rustVersion: "1.70" },
        { version: "1.1.0", yanked: false, rustVersion: "1.91" },
      ],
      mode: "default",
      policyReason: "",
      msrv: "1.91",
    });
    expect(result.status).toBe("update");
    expect(result.reason).toContain("resolved graph still requires exact Rust 1.91 verification");
  });

  test("classifies a release above the declared Rust floor as held", () => {
    const result = classifyCargoCurrency({
      locked: "1.0.0",
      versions: [
        { version: "1.0.0", yanked: false, rustVersion: "1.70" },
        { version: "1.1.0", yanked: false, rustVersion: "1.95" },
      ],
      mode: "default",
      policyReason: "",
      msrv: "1.91",
    });
    expect(result).toMatchObject({ latest: "1.1.0", status: "held" });
  });

  test("does not let an MSRV-blocked latest release hide an actionable update", () => {
    const result = classifyCargoCurrency({
      locked: "1.0.0",
      versions: [
        { version: "1.0.0", yanked: false, rustVersion: "1.70" },
        { version: "1.1.0", yanked: false, rustVersion: "1.80" },
        { version: "1.2.0", yanked: false, rustVersion: "1.95" },
      ],
      mode: "default",
      policyReason: "",
      msrv: "1.91",
    });
    expect(result).toMatchObject({ latest: "1.1.0", status: "update" });
    expect(result.reason).toContain("1.2.0");
    expect(result.reason).toContain("remains held");
  });

  test("fails closed when the sparse index omits the locked crate release", () => {
    const result = classifyCargoCurrency({
      locked: "1.0.0",
      versions: [{ version: "1.1.0", yanked: false, rustVersion: "1.80" }],
      mode: "default",
      policyReason: "",
      msrv: "1.91",
    });
    expect(result.status).toBe("error");
  });

  test("rejects malformed sparse-index schema versions", () => {
    expect(() =>
      parseCratesIndex(
        JSON.stringify({ name: "demo", vers: "1.0.0", yanked: false, v: "3" }),
        "demo"
      )
    ).toThrow("invalid schema version");
  });

  test("maps renamed direct edges to the exact locked registry package", () => {
    const source = "registry+https://github.com/rust-lang/crates.io-index";
    const metadata = {
      version: 1,
      workspace_members: ["root"],
      packages: [
        {
          id: "root",
          rust_version: "1.91",
          dependencies: [{ name: "base64", rename: "base64_std", source, optional: false }],
        },
        { id: "base64-old", name: "base64", version: "0.22.0", source },
        { id: "base64-new", name: "base64", version: "0.23.1", source },
      ],
      resolve: {
        nodes: [
          {
            id: "root",
            deps: [{ name: "base64_std", pkg: "base64-new", dep_kinds: [{ kind: null }] }],
          },
          { id: "base64-old", deps: [] },
          { id: "base64-new", deps: [] },
        ],
      },
    };
    expect(collectCargoTargets(metadata)).toEqual({
      msrv: "1.91",
      targets: [{ name: "base64", locked: "0.23.1", kinds: ["normal"] }],
    });
  });
});

describe("GitHub Actions pins", () => {
  const workflow = [
    "steps:",
    `  - uses: actions/checkout@${SHA_ONE} # v7.0.1`,
    "  - uses: ./local-action",
    "  - uses: docker://alpine:3.20",
  ].join("\n");

  test("collects immutable external actions and ignores local or Docker actions", () => {
    const parsed = parseActionReferences([{ path: ".github/workflows/ci.yml", content: workflow }]);
    expect(parsed.errors).toEqual([]);
    expect(parsed.actions).toEqual([
      {
        repository: "actions/checkout",
        pin: SHA_ONE,
        declaredTag: "v7.0.1",
        locations: [".github/workflows/ci.yml:2"],
      },
    ]);
  });

  test("flags mutable refs", () => {
    const parsed = parseActionReferences([
      { path: ".github/workflows/ci.yml", content: "- uses: actions/checkout@v7" },
    ]);
    expect(parsed.actions).toEqual([]);
    expect(parsed.errors[0]?.reason).toContain("not pinned");
  });

  test("fails closed on valid YAML uses syntax it cannot parse", () => {
    const parsed = parseActionReferences([
      {
        path: ".github/workflows/ci.yml",
        content: `- { uses: actions/checkout@${SHA_ONE} }`,
      },
    ]);
    expect(parsed.actions).toEqual([]);
    expect(parsed.errors[0]?.reason).toContain("cannot parse safely");
  });

  test("does not mistake a block-scalar command for an action key", () => {
    const parsed = parseActionReferences([
      {
        path: ".github/workflows/ci.yml",
        content: ["- name: Example", "  run: |", "    uses: this is shell text"].join("\n"),
      },
    ]);
    expect(parsed).toEqual({ actions: [], errors: [] });
  });

  test("selects the greatest stable semantic release rather than a later backport", () => {
    const release = inspectGitHubReleases([
      { tag_name: "v6.9.1", draft: false, prerelease: false },
      { tag_name: "v7.0.1", draft: false, prerelease: false },
      { tag_name: "v8.0.0-beta.1", draft: false, prerelease: true },
    ]);
    expect(release.tag).toBe("v7.0.1");
  });

  test("compares the full commit SHA rather than trusting the comment", () => {
    const action = parseActionReferences([{ path: "ci.yml", content: workflow }]).actions[0];
    const release = inspectGitHubReleases([
      { tag_name: "v7.0.1", draft: false, prerelease: false },
    ]);
    expect(action).toBeDefined();
    expect(
      classifyActionCurrency({
        action,
        latestRelease: release,
        declaredSha: inspectResolvedCommitSha(SHA_ONE),
        latestSha: inspectResolvedCommitSha(SHA_TWO),
      }).status
    ).toBe("review");
  });
});

describe("safe reporting and network failures", () => {
  test("escapes Markdown and flattens workflow-command input", () => {
    expect(sanitizeReportField("bad|`x`\n::error::<tag>[link]")).toBe(
      "bad\\|\\`x\\` ::error::&lt;tag&gt;\\[link\\]"
    );
  });

  test("uses distinct exit codes for maintenance and monitor errors", () => {
    const row = {
      ecosystem: "npm",
      dependency: "demo",
      locked: "1.0.0",
      latest: "1.0.1",
      reason: "test",
    };
    expect(dependencyWatchExitCode([{ ...row, status: "current" }])).toBe(0);
    expect(dependencyWatchExitCode([{ ...row, status: "held" }])).toBe(0);
    expect(dependencyWatchExitCode([{ ...row, status: "update" }])).toBe(1);
    expect(dependencyWatchExitCode([{ ...row, status: "review" }])).toBe(1);
    expect(dependencyWatchExitCode([{ ...row, status: "error" }])).toBe(2);
  });

  test("renders the security and no-branch boundary in every report", () => {
    const report = renderDependencyReport([
      {
        ecosystem: "npm",
        dependency: "demo",
        locked: "1.0.0",
        latest: "1.0.0",
        status: "current",
        reason: "Current.",
      },
    ]);
    expect(report).toContain("Dependabot Alerts");
    expect(report).toContain("cannot push, create branches, or open pull requests");
  });

  test("puts actionable rows before current rows", () => {
    const report = renderDependencyReport([
      {
        ecosystem: "Cargo",
        dependency: "current-demo",
        locked: "1.0.0",
        latest: "1.0.0",
        status: "current",
        reason: "Current.",
      },
      {
        ecosystem: "npm",
        dependency: "update-demo",
        locked: "1.0.0",
        latest: "1.0.1",
        status: "update",
        reason: "Update.",
      },
    ]);
    expect(report.indexOf("update-demo")).toBeLessThan(report.indexOf("current-demo"));
  });

  test("retries a bounded transient failure without printing its body", async () => {
    const fetchImpl = vi
      .fn()
      .mockResolvedValueOnce(
        new Response("secret upstream failure", { status: 500, headers: { "retry-after": "0" } })
      )
      .mockResolvedValueOnce(new Response("ok", { status: 200 }));
    const sleep = vi.fn().mockResolvedValue(undefined);
    await expect(
      fetchTextWithPolicy("https://example.invalid", { fetchImpl, sleep })
    ).resolves.toBe("ok");
    expect(fetchImpl).toHaveBeenCalledTimes(2);
  });

  test("rejects oversized response bodies", async () => {
    const fetchImpl = vi
      .fn()
      .mockResolvedValue(
        new Response("too large", { status: 200, headers: { "content-length": "9" } })
      );
    await expect(
      fetchTextWithPolicy("https://example.invalid", { fetchImpl, maxBytes: 8, attempts: 1 })
    ).rejects.toThrow("response-size safety cap");
  });

  test("does not follow redirects carrying request credentials", async () => {
    const fetchImpl = vi
      .fn()
      .mockResolvedValue(
        new Response(null, { status: 302, headers: { location: "https://attacker.invalid" } })
      );
    await expect(
      fetchTextWithPolicy("https://api.github.com/example", {
        fetchImpl,
        headers: { Authorization: "Bearer test" },
        attempts: 1,
      })
    ).rejects.toThrow("HTTP 302");
    expect(fetchImpl.mock.calls[0]?.[1]).toMatchObject({ redirect: "manual" });
  });
});

describe("repository no-branch contract", () => {
  test("keeps the workflow read-only and disables routine Dependabot branches", async () => {
    const workflowText = await readFile(
      new URL(".github/workflows/dependency-currency.yml", repositoryRoot),
      "utf8"
    );
    const dependabotText = await readFile(
      new URL(".github/dependabot.yml", repositoryRoot),
      "utf8"
    );

    /** @param {string} name */
    const topLevelBlock = (name) => {
      const lines = workflowText.split(/\r?\n/u);
      const start = lines.indexOf(`${name}:`);
      expect(start).toBeGreaterThanOrEqual(0);
      const block = [];
      for (const line of lines.slice(start + 1)) {
        if (line !== "" && !line.startsWith(" ")) break;
        block.push(line);
      }
      return block;
    };
    const triggerKeys = topLevelBlock("on")
      .filter((line) => /^ {2}[a-z_]+:/u.test(line))
      .map((line) => line.trim().replace(/:$/u, ""));
    const permissionLines = topLevelBlock("permissions").filter((line) => line.trim() !== "");

    expect(triggerKeys).toEqual(["schedule", "workflow_dispatch"]);
    expect(workflowText.match(/^[\t ]*permissions:/gmu)).toEqual(["permissions:"]);
    expect(permissionLines).toEqual(["  contents: read"]);
    expect(workflowText).toContain("persist-credentials: false");
    expect(workflowText).not.toMatch(/permissions:\s*write-all/u);
    expect(dependabotText.match(/open-pull-requests-limit:\s*0/gu)).toHaveLength(3);
    expect(dependabotText).not.toMatch(/open-pull-requests-limit:\s*[1-9]/u);
  });
});

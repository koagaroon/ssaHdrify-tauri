import { resolve } from "node:path";
import { describe, expect, test, vi } from "vitest";

import {
  inspectReleaseDependencyStatus,
  releaseDependencyExitCode,
  renderReleaseDependencyReport,
} from "./release-dependency-status.mjs";

const SOURCE = "a".repeat(40);
const RELEASE = "b".repeat(40);
const TAG = "c".repeat(40);
const API = "https://api.github.com/repos/example/product";
const PUBLISHED = {
  tag_name: "v1.2.3",
  draft: false,
  prerelease: false,
  published_at: "2026-01-01T00:00:00Z",
};

function fixture() {
  const projectRoot = resolve("release-comparison-fixture");
  /** @type {Record<string, {status: number, body: unknown}>} */
  const responses = {
    [`${API}/releases/latest`]: { status: 200, body: PUBLISHED },
    [`${API}/git/ref/tags/v1.2.3`]: {
      status: 200,
      body: { ref: "refs/tags/v1.2.3", object: { type: "commit", sha: RELEASE } },
    },
  };
  /** @type {Record<string, string>} */
  const releasedFiles = {};
  /** @type {Record<string, string>} */
  const currentFiles = {};
  const state = {
    source: SOURCE,
    sourceAtEnd: SOURCE,
    status: "",
    statusAtEnd: "",
    shallow: false,
    missingCommit: false,
    releaseIsAncestor: true,
    sourceIsAncestor: false,
    ancestryError: false,
    /** @type {string[]} */
    changes: [],
    /** @type {string[]} */
    untracked: [],
  };
  let headReads = 0;
  let statusReads = 0;
  const runGit = vi.fn(async (args) => {
    if (args[0] === "rev-parse" && args[1] === "--verify") {
      headReads += 1;
      return { code: 0, stdout: `${headReads === 1 ? state.source : state.sourceAtEnd}\n` };
    }
    if (args[0] === "rev-parse") return { code: 0, stdout: `${state.shallow}\n` };
    if (args[0] === "status") {
      statusReads += 1;
      return { code: 0, stdout: statusReads === 1 ? state.status : state.statusAtEnd };
    }
    if (args[0] === "cat-file") return { code: state.missingCommit ? 128 : 0, stdout: "commit\n" };
    if (args[0] === "merge-base") {
      return {
        code: state.ancestryError
          ? 128
          : (args[2] === RELEASE ? state.releaseIsAncestor : state.sourceIsAncestor)
            ? 0
            : 1,
        stdout: "",
      };
    }
    if (args[0] === "diff")
      return { code: 0, stdout: state.changes.map((path) => `${path}\0`).join("") };
    if (args[0] === "ls-files")
      return { code: 0, stdout: state.untracked.map((path) => `${path}\0`).join("") };
    if (args[0] === "ls-tree") {
      const path = args.at(-1);
      return {
        code: 0,
        stdout: path in releasedFiles ? `100644 blob ${"d".repeat(40)}\t${path}\0` : "",
      };
    }
    if (args[0] === "show") {
      const path = args[1].slice(41);
      return { code: path in releasedFiles ? 0 : 128, stdout: releasedFiles[path] ?? "" };
    }
    throw new Error(`Unexpected test Git command: ${args[0]}`);
  });
  const fetchImpl = vi.fn(async (input) => {
    const response = responses[String(input)];
    if (response === undefined) throw new Error("Unexpected test HTTP request");
    return new Response(JSON.stringify(response.body), { status: response.status });
  });
  const readFileImpl = vi.fn(async (path) => {
    if (!(path in currentFiles))
      throw Object.assign(new Error("Fixture file missing"), { code: "ENOENT" });
    return currentFiles[path];
  });
  return {
    state,
    responses,
    runGit,
    fetchImpl,
    readFileImpl,
    options: { projectRoot, repository: "example/product", runGit, fetchImpl, readFileImpl },
    /** @param {string} path @param {string} previous @param {string} current */
    change(path, previous, current) {
      state.changes.push(path);
      releasedFiles[path] = previous;
      currentFiles[resolve(projectRoot, path)] = current;
    },
  };
}

describe("published stable-release baseline", () => {
  test("resolves an exact lightweight tag and reports the exact source/release commits", async () => {
    const context = fixture();
    const result = await inspectReleaseDependencyStatus(context.options);
    expect(result).toMatchObject({
      status: "compared",
      releaseTag: "v1.2.3",
      releaseTagObject: RELEASE,
      releaseCommit: RELEASE,
      sourceCommit: SOURCE,
      sourceDirty: false,
      relationship: "source-ahead",
      changes: { graph: [], build: [] },
    });
    expect(releaseDependencyExitCode(result)).toBe(0);
    expect(context.fetchImpl.mock.calls.map(([url]) => url)).toEqual([
      `${API}/releases/latest`,
      `${API}/git/ref/tags/v1.2.3`,
    ]);
    expect(
      context.runGit.mock.calls.some(([args]) =>
        ["fetch", "checkout", "tag", "update-ref"].includes(args[0])
      )
    ).toBe(false);
  });

  test("peels an annotated tag instead of using release target_commitish", async () => {
    const context = fixture();
    context.responses[`${API}/releases/latest`].body = {
      ...PUBLISHED,
      target_commitish: "moving-default-branch",
    };
    context.responses[`${API}/git/ref/tags/v1.2.3`].body = {
      ref: "refs/tags/v1.2.3",
      object: { type: "tag", sha: TAG },
    };
    context.responses[`${API}/git/tags/${TAG}`] = {
      status: 200,
      body: { sha: TAG, object: { type: "commit", sha: RELEASE } },
    };
    expect(await inspectReleaseDependencyStatus(context.options)).toMatchObject({
      status: "compared",
      releaseTagObject: TAG,
      releaseCommit: RELEASE,
    });
  });

  test.each([8, 9])(
    "bounds annotated tag peeling at eight objects (%i supplied)",
    async (length) => {
      const context = fixture();
      const tags = Array.from({ length }, (_, index) => (index + 1).toString(16).padStart(40, "0"));
      context.responses[`${API}/git/ref/tags/v1.2.3`].body = {
        ref: "refs/tags/v1.2.3",
        object: { type: "tag", sha: tags[0] },
      };
      tags.forEach((sha, index) => {
        context.responses[`${API}/git/tags/${sha}`] = {
          status: 200,
          body: {
            sha,
            object:
              index === tags.length - 1
                ? { type: "commit", sha: RELEASE }
                : { type: "tag", sha: tags[index + 1] },
          },
        };
      });
      const result = await inspectReleaseDependencyStatus(context.options);
      expect(result.status).toBe(length === 8 ? "compared" : "error");
      expect(context.fetchImpl.mock.calls.length).toBe(10);
    }
  );

  test("rejects an annotated tag cycle", async () => {
    const context = fixture();
    context.responses[`${API}/git/ref/tags/v1.2.3`].body = {
      ref: "refs/tags/v1.2.3",
      object: { type: "tag", sha: TAG },
    };
    context.responses[`${API}/git/tags/${TAG}`] = {
      status: 200,
      body: { sha: TAG, object: { type: "tag", sha: TAG } },
    };
    expect((await inspectReleaseDependencyStatus(context.options)).reason).toContain("cyclic");
    expect(context.fetchImpl).toHaveBeenCalledTimes(3);
  });

  test.each([
    { body: [] },
    { body: [{ ...PUBLISHED, prerelease: true }] },
    { body: [{ ...PUBLISHED, draft: true, published_at: null }] },
  ])(
    "confirms no published stable release only after a successful listing: %j",
    async ({ body }) => {
      const context = fixture();
      context.responses[`${API}/releases/latest`] = { status: 404, body: { message: "Not Found" } };
      context.responses[`${API}/releases?per_page=100&page=1`] = { status: 200, body };
      const result = await inspectReleaseDependencyStatus(context.options);
      expect(result.status).toBe("no-release");
      expect(result.releaseCommit).toBeNull();
      expect(result.sourceCommit).toBe(SOURCE);
      expect(releaseDependencyExitCode(result)).toBe(0);
    }
  );

  test("does not equate an inaccessible repository with no releases", async () => {
    const context = fixture();
    context.responses[`${API}/releases/latest`] = { status: 404, body: {} };
    context.responses[`${API}/releases?per_page=100&page=1`] = {
      status: 404,
      body: { message: "private diagnostic" },
    };
    const result = await inspectReleaseDependencyStatus(context.options);
    expect(result.status).toBe("error");
    expect(releaseDependencyExitCode(result)).toBe(2);
    expect(result.reason).toContain("HTTP 404");
    expect(result.reason).not.toContain("private diagnostic");
  });

  test("rejects contradictory latest and release-list responses", async () => {
    const context = fixture();
    context.responses[`${API}/releases/latest`] = { status: 404, body: {} };
    context.responses[`${API}/releases?per_page=100&page=1`] = { status: 200, body: [PUBLISHED] };
    expect((await inspectReleaseDependencyStatus(context.options)).reason).toContain("disagrees");
  });

  test("reports incomplete absence verification when release history exceeds its bound", async () => {
    const context = fixture();
    context.responses[`${API}/releases/latest`] = { status: 404, body: {} };
    for (let page = 1; page <= 3; page += 1)
      context.responses[`${API}/releases?per_page=100&page=${page}`] = {
        status: 200,
        body: Array.from({ length: 100 }, () => ({ ...PUBLISHED, prerelease: true })),
      };
    const result = await inspectReleaseDependencyStatus(context.options);
    expect(result.status).toBe("error");
    expect(result.reason).toContain("300-release");
    expect(context.fetchImpl).toHaveBeenCalledTimes(4);
  });

  test.each([
    null,
    {},
    { ...PUBLISHED, draft: null },
    { ...PUBLISHED, published_at: null },
    { ...PUBLISHED, prerelease: true },
    { ...PUBLISHED, tag_name: "bad\n::error::tag" },
  ])("rejects malformed or non-stable latest metadata: %j", async (body) => {
    const context = fixture();
    context.responses[`${API}/releases/latest`].body = body;
    expect((await inspectReleaseDependencyStatus(context.options)).status).toBe("error");
    expect(context.fetchImpl).toHaveBeenCalledTimes(1);
  });

  test.each([
    { ref: "refs/tags/v1.2.30", object: { type: "commit", sha: RELEASE } },
    [{ ref: "refs/tags/v1.2.3", object: { type: "commit", sha: RELEASE } }],
    { ref: "refs/tags/v1.2.3", object: { type: "blob", sha: RELEASE } },
    { ref: "refs/tags/v1.2.3", object: { type: "commit", sha: "short" } },
  ])("rejects inexact refs or invalid target objects: %j", async (body) => {
    const context = fixture();
    context.responses[`${API}/git/ref/tags/v1.2.3`].body = body;
    expect((await inspectReleaseDependencyStatus(context.options)).status).toBe("error");
  });

  test("reports network failure without exposing raw errors or request credentials", async () => {
    const context = fixture();
    context.fetchImpl.mockRejectedValue(new Error("secret-token and local path"));
    const result = await inspectReleaseDependencyStatus({
      ...context.options,
      headers: { Authorization: "Bearer secret-token" },
    });
    expect(result.status).toBe("error");
    expect(result.reason).toContain("failed or timed out");
    expect(JSON.stringify(result)).not.toContain("secret-token");
  });
});

describe("source history and dependency/build input classification", () => {
  test("compares a checkout at the release commit without inventing newer source", async () => {
    const context = fixture();
    context.state.source = RELEASE;
    context.state.sourceAtEnd = RELEASE;
    const result = await inspectReleaseDependencyStatus(context.options);
    expect(result).toMatchObject({ status: "compared", relationship: "same", sourceDirty: false });
    expect(context.runGit.mock.calls.some(([args]) => args[0] === "merge-base")).toBe(false);
  });

  test.each([
    [true, "source-behind"],
    [false, "diverged"],
  ])(
    "reports reverse/diverged ancestry without asserting unshipped changes (%s)",
    async (sourceIsAncestor, relationship) => {
      const context = fixture();
      context.state.releaseIsAncestor = false;
      context.state.sourceIsAncestor = sourceIsAncestor;
      const result = await inspectReleaseDependencyStatus(context.options);
      expect(result.relationship).toBe(relationship);
      expect(releaseDependencyExitCode(result)).toBe(0);
      expect(renderReleaseDependencyReport(result)).not.toMatch(
        /not yet shipped|needs? (?:a )?release/iu
      );
    }
  );

  test.each(/** @type {const} */ (["missingCommit", "shallow", "ancestryError"]))(
    "fails explicitly when local history cannot prove the baseline: %s",
    async (condition) => {
      const context = fixture();
      context.state[condition] = true;
      const result = await inspectReleaseDependencyStatus(context.options);
      expect(result.status).toBe("error");
      expect(releaseDependencyExitCode(result)).toBe(2);
      if (condition === "missingCommit")
        expect(result.reason).toContain("released commit is unavailable locally");
    }
  );

  test("keeps workflow and compiler-only differences in the build group", async () => {
    const context = fixture();
    context.state.changes = [".github/workflows/ci.yml", "rust-toolchain.toml"];
    const result = await inspectReleaseDependencyStatus(context.options);
    expect(result.changes).toEqual({
      graph: [],
      build: [".github/workflows/ci.yml", "rust-toolchain.toml"],
    });
    expect(releaseDependencyExitCode(result)).toBe(0);
  });

  test("recognizes Node/npm and Rust build-requirement changes without a dependency-graph change", async () => {
    const context = fixture();
    context.change(
      "package.json",
      JSON.stringify({
        engines: { node: "^24.21.0" },
        packageManager: "npm@12.2.0",
        dependencies: { react: "19.3.0" },
      }),
      JSON.stringify({
        dependencies: { react: "19.3.0" },
        engines: { node: "^24.22.1" },
        packageManager: "npm@12.2.1",
      })
    );
    context.change(
      "package-lock.json",
      JSON.stringify({
        packages: {
          "": { engines: { node: "^24.21.0" }, dependencies: { react: "19.3.0" } },
          "node_modules/react": { version: "19.3.0" },
        },
      }),
      JSON.stringify({
        packages: {
          "": { engines: { node: "^24.22.1" }, dependencies: { react: "19.3.0" } },
          "node_modules/react": { version: "19.3.0" },
        },
      })
    );
    context.change(
      "src-tauri/Cargo.toml",
      '[package]\nname = "app"\nrust-version = "1.91"\n[dependencies]\na = "1.0"\n',
      '[package]\nname = "app"\nrust-version = "1.99.1"\n[dependencies]\na = "1.0"\n'
    );
    const result = await inspectReleaseDependencyStatus(context.options);
    expect(result.changes).toEqual({
      graph: [],
      build: ["package-lock.json", "package.json", "src-tauri/Cargo.toml"],
    });
  });

  test("retains dependency and vendored-source changes as mixed graph inputs", async () => {
    const context = fixture();
    context.change(
      "package.json",
      '{"engines":{"node":"^24.21.0"},"dependencies":{"react":"19.2.0"}}',
      '{"engines":{"node":"^24.22.1"},"dependencies":{"react":"19.3.0"}}'
    );
    context.change(
      "package-lock.json",
      '{"packages":{"":{"engines":{"node":"^24.21.0"}},"node_modules/react":{"version":"19.2.0"}}}',
      '{"packages":{"":{"engines":{"node":"^24.22.1"}},"node_modules/react":{"version":"19.3.0"}}}'
    );
    context.state.changes.push("src-tauri/vendor/libsqlite3-sys/sqlite3/sqlite3.c");
    const result = await inspectReleaseDependencyStatus(context.options);
    expect(result.changes.graph).toHaveLength(3);
    expect(result.changes.build).toEqual([]);
    expect(renderReleaseDependencyReport(result)).toContain(
      "do not prove a runtime dependency changed"
    );
  });

  test("distinguishes dirty current inputs and untracked workflows from source HEAD", async () => {
    const context = fixture();
    context.state.source = RELEASE;
    context.state.sourceAtEnd = RELEASE;
    context.state.status = "?? .github/workflows/new.yml\0";
    context.state.statusAtEnd = context.state.status;
    context.state.untracked = [".github/workflows/new.yml"];
    const result = await inspectReleaseDependencyStatus(context.options);
    expect(result).toMatchObject({
      status: "compared",
      relationship: "same",
      sourceDirty: true,
      changes: { graph: [], build: [".github/workflows/new.yml"] },
    });
    expect(renderReleaseDependencyReport(result)).toContain("current files include local changes");
  });

  test("treats a newly added manifest as a mixed input", async () => {
    const context = fixture();
    context.state.untracked = ["package.json"];
    const result = await inspectReleaseDependencyStatus(context.options);
    expect(result.changes.graph).toEqual(["package.json"]);
    expect(context.readFileImpl).not.toHaveBeenCalled();
  });

  test("does not claim a complete snapshot after HEAD or working-tree membership changes", async () => {
    for (const condition of /** @type {const} */ (["sourceAtEnd", "statusAtEnd"])) {
      const context = fixture();
      context.state[condition] = condition === "sourceAtEnd" ? "e".repeat(40) : " M package.json\0";
      const result = await inspectReleaseDependencyStatus(context.options);
      expect(result.status).toBe("error");
      expect(result.reason).toContain("changed during comparison");
    }
  });

  test("reports malformed compared manifests as incomplete", async () => {
    const context = fixture();
    context.change("package.json", "{}", "{");
    expect((await inspectReleaseDependencyStatus(context.options)).reason).toContain(
      "invalid JSON"
    );
  });

  test("reports an unreadable compared manifest without exposing filesystem errors", async () => {
    const context = fixture();
    context.change("package.json", "{}", "{}");
    context.readFileImpl.mockRejectedValue(new Error("private filesystem detail"));
    const result = await inspectReleaseDependencyStatus(context.options);
    expect(result.status).toBe("error");
    expect(result.reason).toContain("could not be read safely");
    expect(result.reason).not.toContain("private filesystem detail");
  });

  test.each([256, 257])(
    "bounds changed-input collection at 256 files (%i supplied)",
    async (count) => {
      const context = fixture();
      context.state.changes = Array.from(
        { length: count },
        (_, index) => `.github/workflows/${index}.yml`
      );
      const result = await inspectReleaseDependencyStatus(context.options);
      expect(result.status).toBe(count === 256 ? "compared" : "error");
      if (count === 256) expect(result.changes.build).toHaveLength(256);
    }
  );
});

describe("bounded release reporting", () => {
  test("escapes external Markdown and keeps workflow-command text on its existing line", async () => {
    const context = fixture();
    const result = await inspectReleaseDependencyStatus(context.options);
    result.releaseTag = "bad|`tag`\n::error::<markup>[link]";
    result.reason = "x".repeat(10000);
    result.changes.build = [".github/workflows/a|`x`\n::warning::<tag>[link]\u001b[31m.yml"];
    const report = renderReleaseDependencyReport(result);
    expect(report).toContain("bad\\|\\`tag\\`");
    expect(report).not.toMatch(/^::(?:error|warning)::/mu);
    expect(report).not.toContain("<markup>");
    expect(report).not.toContain("\u001b");
    expect(report.length).toBeLessThan(3000);
    expect(report).toContain("does not inspect release binaries");
    expect(report).toContain("Input differences alone do not make this monitor fail");
  });

  test("rejects report arrays over the input limit", async () => {
    const context = fixture();
    const result = await inspectReleaseDependencyStatus(context.options);
    result.changes.build = Array(257).fill("rust-toolchain.toml");
    expect(() => renderReleaseDependencyReport(result)).toThrow("file limit");
  });
});

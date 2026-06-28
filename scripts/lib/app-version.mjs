// Shared app-version resolver used by both `vite.config.ts` (GUI bundle
// __APP_VERSION__) and `scripts/build-engine.mjs` (CLI engine bundle
// __APP_VERSION__). Pure stdlib — no Vite / esbuild dependencies — so
// the esbuild-only build path doesn't drag in the GUI bundler's
// transitive deps.
//
// Why a shared module: the
// resolver was duplicated in vite.config.ts and build-engine.mjs.
// The old split resolver surfaced one already-drifted gap (build-engine.mjs
// lacked the sanitize-emptied warn branch vite.config.ts had —
// `v0.0.0-unknown` would have shipped on the CLI side while the GUI
// surfaced a warning) and one bug class (empty-string
// `pkg.version` silently fell through to the sentinel rather than
// surfacing the package.json corruption). Single source of truth here
// closes both at once and prevents future drift.
//
// Use `execFileSync` (no shell interpretation)
// instead of `execSync` to remove the build-time injection surface
// where an attacker who can plant `git.bat` / `git.exe` earlier on
// PATH could execute arbitrary code via the shell. Removes the
// `shell: true` default on Windows; the git binary path is resolved
// by the OS but no shell metacharacter expansion happens on the
// command line.

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

/**
 * Resolve the app version string for build-time injection.
 *
 * Order of preference:
 *   1. `git describe --tags --dirty --always` — exact tag (e.g.
 *      `v1.1.0-preview.5`), appends `-dirty` if the working tree has
 *      uncommitted changes, falls back to a short commit hash if no
 *      tag is reachable.
 *   2. `v<package.json .version>` — used when git is unavailable
 *      (source zip, CI without git history, etc.)
 *   3. `v0.0.0-unknown` — last-resort sentinel; should never surface
 *      in practice.
 *
 * @param {string} projectRoot - Absolute path to the repo root (the
 *   directory containing `package.json`). vite.config.ts passes its
 *   own `__dirname`; build-engine.mjs passes `resolve(__dirname, "..")`.
 * @returns {string} sanitized version label.
 */
export function resolveAppVersion(projectRoot) {
  const raw = readGitDescribe(projectRoot) ?? readPackageVersion(projectRoot) ?? "v0.0.0-unknown";

  // Defense-in-depth: git tag names can contain most bytes, and the
  // raw string is injected into the bundle as `__APP_VERSION__`. We
  // render it through React text nodes today (safe), but a future
  // refactor that passes this value to `dangerouslySetInnerHTML` or
  // a `<meta>` content attribute could become an injection vector.
  // Restrict to alphanumerics / punctuation that legitimate version
  // strings use.
  const sanitized = raw.replace(/[^a-zA-Z0-9._+-]/g, "");
  if (raw && !sanitized) {
    // Parity fix: build-engine.mjs previously
    // lacked this branch; the GUI would warn while the CLI silently
    // used `v0.0.0-unknown`. Single helper means both sides surface
    // the same way now.
    console.warn(
      `[app-version] resolveAppVersion: raw value "${raw}" sanitized to empty; ` +
        `falling back to v0.0.0-unknown`
    );
  }
  return sanitized || "v0.0.0-unknown";
}

/**
 * @param {unknown} err
 * @returns {string}
 */
function describeError(err) {
  if (err instanceof Error) return err.message;
  return String(err);
}

/**
 * @param {string} projectRoot
 * @returns {string | null}
 */
function readGitDescribe(projectRoot) {
  try {
    return execFileSync("git", ["describe", "--tags", "--dirty", "--always"], {
      stdio: ["pipe", "pipe", "pipe"],
      // Cap the build-time git lookup so a pathological repo (huge
      // packed refs, credential-prompt, LFS hang) can't stall the
      // build forever. The catch arm now surfaces
      // the underlying error via console.warn so a real failure
      // (corrupt repo, timeout) doesn't get masked as "git
      // unavailable" silently.
      timeout: 5000,
      cwd: projectRoot,
    })
      .toString()
      .trim();
  } catch (err) {
    console.warn(`[app-version] git describe unavailable: ${describeError(err)}`);
    return null;
  }
}

/**
 * @param {string} projectRoot
 * @returns {string | null}
 */
function readPackageVersion(projectRoot) {
  try {
    const pkg = JSON.parse(readFileSync(resolve(projectRoot, "package.json"), "utf8"));
    // Explicit non-empty check instead of just
    // truthy. A package.json with `"version": ""` would
    // collapse via falsy-fallthrough to the sentinel without
    // surfacing the corruption.
    if (typeof pkg.version === "string" && pkg.version.length > 0) {
      return `v${pkg.version}`;
    }
    console.warn(
      `[app-version] package.json version field is missing or empty; ` +
        `falling back to v0.0.0-unknown sentinel`
    );
    return null;
  } catch (err) {
    console.warn(`[app-version] package.json unreadable: ${describeError(err)}`);
    return null;
  }
}

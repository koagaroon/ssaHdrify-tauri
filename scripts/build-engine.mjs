// Build helper for the CLI engine bundle (esbuild + __APP_VERSION__ define).
//
// Why this exists (R13 W13.1 / N-R13-9): the previous inline `esbuild`
// invocation in `package.json::build:engine` produced a bundle that left
// `__APP_VERSION__` unresolved, because esbuild has no equivalent of
// Vite's `define` injection without an explicit `--define` flag. The
// transitive import chain `cli-engine-entry.ts → font-embedder.ts →
// tauri-api.ts → i18n/strings.ts` drags `footer_version`'s
// `${__APP_VERSION__}` reference into the bundle; at deno_core load
// time, V8 throws `ReferenceError: __APP_VERSION__ is not defined`.
// `tests/test_chain.rs` integration tests were the canary; vitest /
// cargo unit tests don't exercise the deno_core path so the failure
// stayed silent. See ssahdrify-tauri design doc § Roadmap → Active.
//
// Why a helper script (not inline `--define` in package.json): shell
// quoting for `--define:NAME='"value"'` differs between POSIX bash,
// Windows cmd.exe, and PowerShell. Encoding the substitution in JS
// dodges all of that. Also aligns with the project's "version string
// is never hardcoded" lock (design doc § Key constraints #2) — this
// helper resolves the version the same way `vite.config.ts` does, so
// the engine bundle and the GUI bundle agree on `__APP_VERSION__` at
// every build.

import { build } from "esbuild";
import { execSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const projectRoot = resolve(__dirname, "..");

function resolveAppVersion() {
  // Mirror of vite.config.ts::resolveAppVersion (same precedence
  // order + same 5 s git timeout + same sanitization allowlist).
  // Don't `import` from vite.config.ts: that pulls Vite's deps into
  // an esbuild-only build path. Duplicate the ~20 lines instead.
  const raw = (() => {
    try {
      return execSync("git describe --tags --dirty --always", {
        stdio: ["pipe", "pipe", "pipe"],
        timeout: 5000,
        cwd: projectRoot,
      })
        .toString()
        .trim();
    } catch {
      // git unavailable or repo has no commits.
    }
    try {
      const pkg = JSON.parse(readFileSync(resolve(projectRoot, "package.json"), "utf8"));
      if (pkg.version) return `v${pkg.version}`;
    } catch {
      // package.json missing / malformed.
    }
    return "v0.0.0-unknown";
  })();
  const sanitized = raw.replace(/[^a-zA-Z0-9._+-]/g, "");
  return sanitized || "v0.0.0-unknown";
}

const APP_VERSION = resolveAppVersion();

await build({
  entryPoints: [resolve(projectRoot, "src/cli-engine-entry.ts")],
  bundle: true,
  minify: true,
  format: "iife",
  globalName: "ssaHdrifyCliEngine",
  target: "es2020",
  platform: "neutral",
  mainFields: ["module", "main"],
  outfile: resolve(projectRoot, "dist-engine/engine.js"),
  define: {
    __APP_VERSION__: JSON.stringify(APP_VERSION),
  },
});

console.log(`build:engine — bundled with __APP_VERSION__ = ${JSON.stringify(APP_VERSION)}`);

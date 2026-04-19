import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { execSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// ESM-safe equivalent of CommonJS __dirname — Vite injects a shim today, but
// relying on import.meta.url keeps this config portable across strict-ESM
// tooling (and future Vite versions that might drop the shim).
const __dirname = dirname(fileURLToPath(import.meta.url));

/**
 * Version label shown in the app footer.
 *
 * Derived automatically at build time to eliminate the drift class of bugs
 * where a hardcoded version string in i18n/strings.ts silently goes stale.
 *
 * Order of preference:
 *   1. `git describe --tags --dirty --always` — exact tag (e.g. `v1.1.0-preview.5`),
 *      appends `-dirty` if the working tree has uncommitted changes, falls back
 *      to a short commit hash if no tag is reachable.
 *   2. `v<package.json .version>` — used when git is unavailable (source zip,
 *      CI without git history, etc.)
 *   3. `v0.0.0-unknown` — last-resort sentinel; should never surface in practice.
 */
function resolveAppVersion(): string {
  const raw = (() => {
    try {
      return execSync("git describe --tags --dirty --always", {
        stdio: ["pipe", "pipe", "pipe"],
        // Cap the build-time git lookup so a pathological repo (huge packed
        // refs, credential-prompt, LFS hang) can't stall the build forever.
        timeout: 5000,
      })
        .toString()
        .trim();
    } catch {
      // git unavailable or repo has no commits — fall back to npm version.
    }
    try {
      const pkg = JSON.parse(readFileSync(resolve(__dirname, "package.json"), "utf8")) as {
        version?: string;
      };
      if (pkg.version) return `v${pkg.version}`;
    } catch {
      // package.json missing or malformed — fall through to sentinel.
    }
    return "v0.0.0-unknown";
  })();
  // Defense-in-depth: git tag names can contain most bytes, and the raw
  // string is injected into the bundle as `__APP_VERSION__`. We render it
  // through React text nodes today (safe), but a future refactor that
  // passes this value to `dangerouslySetInnerHTML` or a `<meta>` content
  // attribute could become an injection vector. Restrict to the
  // alphanumerics / punctuation that legitimate version strings use.
  const sanitized = raw.replace(/[^a-zA-Z0-9._+-]/g, "");
  return sanitized || "v0.0.0-unknown";
}

const APP_VERSION = resolveAppVersion();

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss()],
  define: {
    __APP_VERSION__: JSON.stringify(APP_VERSION),
  },
  // Bind the dev server to loopback only. Vite's default is already
  // `localhost`, but being explicit guarantees `npm run dev` never exposes
  // the unauth'd hot-reload server on the LAN even if a developer passes
  // --host accidentally.
  server: {
    host: "127.0.0.1",
    strictPort: true,
  },
});

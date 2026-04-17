import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { execSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

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
  try {
    return execSync("git describe --tags --dirty --always", {
      stdio: ["pipe", "pipe", "pipe"],
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
}

const APP_VERSION = resolveAppVersion();

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss()],
  define: {
    __APP_VERSION__: JSON.stringify(APP_VERSION),
  },
});

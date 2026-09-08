import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";
// Shared resolver between this config and scripts/build-engine.mjs.
// See scripts/lib/app-version.mjs for the version-precedence logic.
import { resolveAppVersion } from "./scripts/lib/app-version.mjs";
import { buildFrontendNotices } from "./scripts/lib/frontend-notices.mjs";

// ESM-safe equivalent of CommonJS __dirname — Vite injects a shim today, but
// relying on import.meta.url keeps this config portable across strict-ESM
// tooling (and future Vite versions that might drop the shim).
const __dirname = dirname(fileURLToPath(import.meta.url));

/**
 * Version label shown in the app footer.
 *
 * Derived automatically at build time to eliminate the drift class of bugs
 * where a hardcoded version string in i18n/strings.ts silently goes stale.
 * See `scripts/lib/app-version.mjs` for the resolver implementation +
 * sanitization allowlist + fallback ladder.
 */
const APP_VERSION = resolveAppVersion(__dirname);
const frontendNotices = buildFrontendNotices(__dirname);
const noticesModule = "virtual:frontend-notices";

// https://vite.dev/config/
export default defineConfig({
  plugins: [
    react(),
    tailwindcss(),
    {
      name: "frontend-notices",
      resolveId(id) {
        if (id === noticesModule) return "\0" + noticesModule;
      },
      load(id) {
        if (id === "\0" + noticesModule) return `export default ${JSON.stringify(frontendNotices)}`;
      },
      generateBundle() {
        this.emitFile({
          type: "asset",
          fileName: "third-party-notices.txt",
          source: frontendNotices,
        });
      },
    },
  ],
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

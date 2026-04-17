/// <reference types="vite/client" />

/**
 * Build-time constant injected by vite.config.ts — see `resolveAppVersion()`.
 * Resolves to the most recent git tag (e.g. `v1.1.0-preview.5`), or a short
 * commit hash when no tag is reachable. Never undefined at runtime.
 */
declare const __APP_VERSION__: string;

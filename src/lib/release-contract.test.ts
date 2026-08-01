/// <reference types="node" />

import { readFile } from "node:fs/promises";
import { describe, expect, it } from "vitest";

async function readText(relativePath: string): Promise<string> {
  return readFile(new URL(relativePath, import.meta.url), "utf8");
}

describe("release configuration contract", () => {
  it("keeps manifest and lockfile versions synchronized", async () => {
    const packageJson = JSON.parse(await readText("../../package.json")) as {
      version: string;
    };
    const packageLock = JSON.parse(await readText("../../package-lock.json")) as {
      version: string;
      packages: Record<string, { version?: string }>;
    };
    const cargoToml = await readText("../../src-tauri/Cargo.toml");
    const cargoLock = await readText("../../src-tauri/Cargo.lock");
    const cargoManifestVersion = /^version = "([^"]+)"$/mu.exec(cargoToml)?.[1];
    const cargoLockVersion = /\[\[package\]\]\r?\nname = "ssahdrify"\r?\nversion = "([^"]+)"/u.exec(
      cargoLock
    )?.[1];

    expect(packageLock.version).toBe(packageJson.version);
    expect(packageLock.packages[""]?.version).toBe(packageJson.version);
    expect(cargoManifestVersion).toBe(packageJson.version);
    expect(cargoLockVersion).toBe(packageJson.version);
  });

  it("pins the native GUI and CLI release build composition", async () => {
    const packageJson = JSON.parse(await readText("../../package.json")) as {
      scripts: Record<string, string>;
    };
    const tauriConfig = JSON.parse(await readText("../../src-tauri/tauri.conf.json")) as {
      build: { beforeBuildCommand: string };
    };
    const cargoToml = await readText("../../src-tauri/Cargo.toml");

    expect(packageJson.scripts["build:all"]).toBe("npm run tauri build && npm run build:cli");
    expect(packageJson.scripts.tauri).toBe("tauri");
    expect(packageJson.scripts.build).toBe("npm run typecheck:ts7 && vite build");
    expect(packageJson.scripts["build:engine"]).toBe("node scripts/build-engine.mjs");
    expect(packageJson.scripts["build:cli"]).toBe(
      "npm run typecheck:ts7 && npm run build:engine && cargo build --release --bin ssahdrify-cli --manifest-path src-tauri/Cargo.toml"
    );
    expect(tauriConfig.build.beforeBuildCommand).toBe("npm run build");
    expect(cargoToml).toMatch(/^default-run = "ssahdrify"$/mu);
    expect(cargoToml).toMatch(
      /\[\[bin\]\]\r?\nname = "ssahdrify-cli"\r?\npath = "src\/bin\/cli\/main\.rs"/u
    );
  });
});

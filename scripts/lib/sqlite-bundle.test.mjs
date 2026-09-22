import { createHash } from "node:crypto";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, test } from "vitest";

const vendor = fileURLToPath(new URL("../../src-tauri/vendor/", import.meta.url));
const payload = join(vendor, "libsqlite3-sys");
const provenance = JSON.parse(readFileSync(join(vendor, "sqlite-provenance.json"), "utf8"));

/** @param {string} directory @returns {string[]} */
function filesIn(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) =>
    entry.isDirectory()
      ? filesIn(join(directory, entry.name)).map((file) => `${entry.name}/${file}`)
      : [entry.name]
  );
}

describe("bundled SQLite provenance", () => {
  test("preserves the complete reviewed native payload byte for byte", () => {
    expect(filesIn(payload).sort()).toEqual(Object.keys(provenance.files).sort());
    for (const [file, hashes] of Object.entries(provenance.files)) {
      const digest = createHash("sha256")
        .update(readFileSync(join(payload, file)))
        .digest("hex");
      expect(digest, file).toBe(hashes.sha256);
    }
    expect(
      Object.entries(provenance.files)
        .filter(([, hashes]) => hashes.sha256 !== hashes.upstreamSha256)
        .map(([file]) => file)
        .sort()
    ).toEqual([
      "sqlite3/bindgen_bundled_version.rs",
      "sqlite3/bindgen_bundled_version_ext.rs",
      "sqlite3/sqlite3.c",
      "sqlite3/sqlite3.h",
    ]);
  });

  test("ships the official SQLite amalgamation with matching source identity", () => {
    const source = readFileSync(join(payload, "sqlite3/sqlite3.c"));
    expect(createHash("sha3-256").update(source).digest("hex")).toBe(
      provenance.sqlite.sourceSha3_256
    );
    const header = readFileSync(join(payload, "sqlite3/sqlite3.h"), "utf8");
    expect(/^#define SQLITE_VERSION\s+"([^"]+)"$/mu.exec(header)?.[1]).toBe(
      provenance.sqlite.version
    );
    expect(/^#define SQLITE_SOURCE_ID\s+"([^"]+)"$/mu.exec(header)?.[1]).toBe(
      provenance.sqlite.sourceId
    );
  });
});

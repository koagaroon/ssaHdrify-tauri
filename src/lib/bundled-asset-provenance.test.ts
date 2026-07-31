/// <reference types="node" />

import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { describe, expect, it } from "vitest";

const BUNDLED_ASSET_PROVENANCE = [
  {
    name: "InterVariable.woff2",
    path: new URL("../assets/fonts/inter/InterVariable.woff2", import.meta.url),
    sha256: "693b77d4f32ee9b8bfc995589b5fad5e99adf2832738661f5402f9978429a8e3",
    upstreamBlob: "5a8d3e72ad7ffb62af3b146e1b1f54ab5813a212",
    source:
      "https://api.github.com/repos/rsms/inter/git/blobs/5a8d3e72ad7ffb62af3b146e1b1f54ab5813a212",
  },
  {
    name: "Inter LICENSE.txt",
    path: new URL("../assets/fonts/inter/LICENSE.txt", import.meta.url),
    sha256: "262481e844521b326f5ecd053e59b98c8b2da78c8ee1bdbb6e8174305e54935a",
    upstreamBlob: "9b2ca37b3ffc77391d8b2ebef4a974ef32bf46ea",
    source:
      "https://api.github.com/repos/rsms/inter/git/blobs/9b2ca37b3ffc77391d8b2ebef4a974ef32bf46ea",
  },
  {
    name: "SmileySans-Oblique.ttf.woff2",
    path: new URL("../assets/fonts/smiley-sans/SmileySans-Oblique.ttf.woff2", import.meta.url),
    sha256: "731f22973349404b15a88a99ef3b5dd4104c0965c23b7e485c1f11e84fea99e2",
    upstreamBlob: "0da5a7f1ba45b1f494a822bba85a41bbc676e578",
    source: "https://github.com/atelier-anchor/smiley-sans/releases/tag/v2.0.1",
  },
  {
    name: "Smiley Sans LICENSE.txt",
    path: new URL("../assets/fonts/smiley-sans/LICENSE.txt", import.meta.url),
    sha256: "9401f4050f1b66c26b6ccdc8b0e14a3c1cc37aac122eda84386f25854a9bec72",
    upstreamBlob: "d4511630b94994b2b3eb0c99cec1a11cac804004",
    source:
      "https://api.github.com/repos/atelier-anchor/smiley-sans/git/blobs/d4511630b94994b2b3eb0c99cec1a11cac804004",
  },
  {
    name: "Feather Icons LICENSE.txt",
    path: new URL("../assets/licenses/feather-LICENSE.txt", import.meta.url),
    sha256: "308028e93fcf84972523cdf6e616f73168546b4953895f516d01287f16fe7bee",
    upstreamBlob: "1f4f4336baff1185dc78ae9e78cac46628dcc869",
    source:
      "https://api.github.com/repos/feathericons/feather/git/blobs/1f4f4336baff1185dc78ae9e78cac46628dcc869",
  },
] as const;

describe("bundled asset provenance", () => {
  for (const asset of BUNDLED_ASSET_PROVENANCE) {
    it(`keeps ${asset.name} byte-identical to its recorded source`, async () => {
      const bytes = await readFile(asset.path);
      const digest = createHash("sha256").update(bytes).digest("hex");
      // Git's SHA-1 object ID is checked only as upstream identity, not as a security primitive.
      const gitBlobHeader = Buffer.from(`blob ${bytes.byteLength}\0`, "utf8");
      const gitBlob = createHash("sha1").update(gitBlobHeader).update(bytes).digest("hex");

      expect(digest).toBe(asset.sha256);
      expect(gitBlob).toBe(asset.upstreamBlob);
      expect(asset.source).toMatch(/^https:\/\//);
    });
  }
});

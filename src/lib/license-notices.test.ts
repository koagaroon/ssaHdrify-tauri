/// <reference types="node" />

import { createHash } from "node:crypto";
import { describe, expect, it } from "vitest";
import { LICENSE_NOTICES, type LicenseNoticeId } from "./license-notices";

const LEGAL_TEXT_SHA256: Record<LicenseNoticeId, string> = {
  ssahdrify: "605e9047a563c5c8396ffb18232aa4304ec56586aee537c45064c6fb425e44ad",
  inter: "262481e844521b326f5ecd053e59b98c8b2da78c8ee1bdbb6e8174305e54935a",
  "smiley-sans": "9401f4050f1b66c26b6ccdc8b0e14a3c1cc37aac122eda84386f25854a9bec72",
  feather: "308028e93fcf84972523cdf6e616f73168546b4953895f516d01287f16fe7bee",
};

describe("embedded license notices", () => {
  it("preserves every complete legal payload byte-for-byte", () => {
    for (const notice of LICENSE_NOTICES) {
      const digest = createHash("sha256").update(notice.text, "utf8").digest("hex");
      expect(digest).toBe(LEGAL_TEXT_SHA256[notice.id]);
    }
  });

  it("ships the project GPL text and source location", () => {
    const project = LICENSE_NOTICES.find((notice) => notice.id === "ssahdrify");

    expect(project?.licenseId).toBe("GPL-3.0-or-later");
    expect(project?.source).toBe("https://github.com/koagaroon/ssaHdrify-tauri");
    expect(project?.text).toContain("GNU GENERAL PUBLIC LICENSE");
    expect(project?.text).toContain("Version 3, 29 June 2007");
  });

  it("preserves each bundled font's copyright header and complete OFL text", () => {
    const inter = LICENSE_NOTICES.find((notice) => notice.id === "inter");
    const smileySans = LICENSE_NOTICES.find((notice) => notice.id === "smiley-sans");

    expect(inter?.text).toContain("Copyright (c) 2016 The Inter Project Authors");
    expect(smileySans?.text).toContain("Copyright (c) 2022--2024, atelierAnchor");
    expect(smileySans?.text).toContain("Reserved Font Name <Smiley> and <得意黑>");
    for (const notice of [inter, smileySans]) {
      expect(notice?.licenseId).toBe("OFL-1.1");
      expect(notice?.text).toContain("SIL OPEN FONT LICENSE Version 1.1");
      expect(notice?.text).toContain("OTHER DEALINGS IN THE FONT SOFTWARE.");
    }
  });

  it("includes the Feather MIT attribution used by interface glyphs", () => {
    const feather = LICENSE_NOTICES.find((notice) => notice.id === "feather");

    expect(feather?.text).toContain("Copyright (c) 2013-2023 Cole Bemis");
    expect(feather?.text).toContain("The MIT License (MIT)");
    expect(feather?.text).toContain('THE SOFTWARE IS PROVIDED "AS IS"');
  });
});

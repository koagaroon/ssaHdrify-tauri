import { describe, expect, it } from "vitest";
import {
  canonicalLanguageTag,
  extractLangFromBaseName,
  LANG_TAGS,
  subtitleLanguageFromName,
} from "./lang-detection";

describe("lang-detection", () => {
  it("maps all supported language tags to deterministic canonical codes", () => {
    expect(LANG_TAGS.size).toBe(20);
    for (const alias of ["zh", "zh-cn", "chs", "sc", "ZH-CN"]) {
      expect(canonicalLanguageTag(alias)).toBe("sc");
    }
    for (const alias of ["zh-tw", "cht", "tc", "CHT"]) {
      expect(canonicalLanguageTag(alias)).toBe("tc");
    }
    for (const alias of ["ja", "jpn", "jp", "JPN"]) {
      expect(canonicalLanguageTag(alias)).toBe("jp");
    }
    expect(canonicalLanguageTag("eng")).toBe("en");
    expect(canonicalLanguageTag("kor")).toBe("ko");
    expect(canonicalLanguageTag("fr")).toBe("fr");
  });

  it("extracts trailing language tags from basenames while rejecting leading-dot names", () => {
    expect(extractLangFromBaseName("EP01.sc")).toBe("sc");
    expect(extractLangFromBaseName("EP01.1080p.ZH-TW")).toBe("zh-tw");
    expect(extractLangFromBaseName(".zh")).toBe("");
    expect(extractLangFromBaseName("EP01.unknown")).toBe("");
    expect(extractLangFromBaseName("EP01")).toBe("");
  });

  it("derives canonical language tags from full subtitle filenames", () => {
    expect(subtitleLanguageFromName("[Group] Show - 01.chs.ass")).toBe("sc");
    expect(subtitleLanguageFromName("[Group] Show - 01.JPN.srt")).toBe("jp");
    expect(subtitleLanguageFromName("[Group] Show - 01.ass")).toBe("");
  });
});

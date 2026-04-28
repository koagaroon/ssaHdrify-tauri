/**
 * Pairing-engine tests.
 *
 * Anchored to real fan-sub naming captured in the design doc:
 *   - LoliHouse (Pattern A: ` - NN [`)
 *   - Haruhana, Nekomoe&LoliHouse (Pattern A)
 *   - Airota, Nekomoe kissaten (Pattern B: `][NN][`)
 *   - 樱桃花字幕组 (Pattern A, bilingual romaji/Chinese)
 *   - DBD-Raws (Pattern B)
 *
 * The original Western-TV regex set (S\dE\d / EP\d / 第N话) hit ZERO
 * of these seven samples; this suite locks in coverage so a future
 * "simplification" can't quietly regress the fan-sub paths.
 */
import { describe, it, expect } from "vitest";
import {
  parseFilename,
  bracketCleanup,
  extractEpisode,
  extractSeason,
  buildPairings,
  deriveRenameOutputPath,
  isNoOpRename,
  assignSubtitleToRow,
  type PairingRow,
} from "./pairing-engine";

function parse(name: string) {
  return parseFilename("/dummy/" + name, name);
}

describe("bracketCleanup", () => {
  it("strips every bracket group and collapses whitespace", () => {
    expect(bracketCleanup("[A][B] C - 03 [D]")).toBe("C - 03");
  });

  it("returns trimmed even when input is all brackets", () => {
    expect(bracketCleanup("[A][B][C]")).toBe("");
  });

  it("preserves text between brackets", () => {
    expect(bracketCleanup("[Group] Show Name - 03")).toBe("Show Name - 03");
  });
});

describe("extractEpisode — documented fan-sub samples", () => {
  it("Pattern A — LoliHouse Isekai Nonbiri Nouka 2 - 03", () => {
    const name = "[LoliHouse] Isekai Nonbiri Nouka 2 - 03 [WebRip 1080p HEVC-10bit AAC SRTx2].mkv";
    const ep = extractEpisode(name, bracketCleanup(name));
    expect(ep?.episode).toBe(3);
  });

  it("Pattern A — LoliHouse Tensei shitara ... 2nd Season - 24", () => {
    const name =
      "[LoliHouse] Tensei shitara Dainana Ouji Datta node, Kimama ni Majutsu wo Kiwamemasu 2nd Season - 24 [WebRip 1080p HEVC-10bit AAC SRTx2].mkv";
    const ep = extractEpisode(name, bracketCleanup(name));
    expect(ep?.episode).toBe(24);
  });

  it("Pattern A — 樱桃花字幕组 bilingual - 24", () => {
    const name =
      "[樱桃花字幕组]转生为第七王子，随心所欲的魔法学习之路 第二季 Dainanaoji S2 - 24 [1080p][简日内嵌].mp4";
    const ep = extractEpisode(name, bracketCleanup(name));
    expect(ep?.episode).toBe(24);
  });

  it("Pattern A — Haruhana Kamiina Botan - 02", () => {
    const name =
      "[Haruhana] Kamiina Botan, Yoeru Sugata wa Yuri no Hana - 02 [WebRip][HEVC-10bit 1080p][CHI_JPN].mkv";
    const ep = extractEpisode(name, bracketCleanup(name));
    expect(ep?.episode).toBe(2);
  });

  it("Pattern A — Nekomoe&LoliHouse Kamiina Botan - 03", () => {
    const name =
      "[Nekomoe kissaten&LoliHouse] Kamiina Botan, Yoeru Sugata wa Yuri no Hana - 03 [WebRip 1080p HEVC-10bit AAC ASSx2].mkv";
    const ep = extractEpisode(name, bracketCleanup(name));
    expect(ep?.episode).toBe(3);
  });

  it("Pattern B — Airota Kamiina Botan [03]", () => {
    const name =
      "[Airota][Kamiina Botan, Yoeru Sugata wa Yuri no Hana][03][1080p AVC AAC][CHT].mp4";
    const ep = extractEpisode(name, bracketCleanup(name));
    expect(ep?.episode).toBe(3);
  });

  it("Pattern B — Nekomoe kissaten Kamiina Botan [03]", () => {
    const name =
      "[Nekomoe kissaten][Kamiina Botan, Yoeru Sugata wa Yuri no Hana][03][1080p][JPTC].mp4";
    const ep = extractEpisode(name, bracketCleanup(name));
    expect(ep?.episode).toBe(3);
  });

  it("Pattern B — DBD-Raws Isekai Nonbiri Nouka [01]", () => {
    const name = "[DBD-Raws][Isekai Nonbiri Nouka][01][1080P][BDRip][HEVC-10bit][FLAC].sc.ass";
    const ep = extractEpisode(name, bracketCleanup(name));
    expect(ep?.episode).toBe(1);
  });
});

describe("extractEpisode — Western fallbacks", () => {
  it("S01E01 captures both season and episode", () => {
    const name = "Show.S01E05.1080p.WEB-DL.mkv";
    const ep = extractEpisode(name, bracketCleanup(name));
    expect(ep?.episode).toBe(5);
    expect(ep?.seasonFromMatch).toBe(1);
  });

  it("EP01 / E01 catches Western-ish naming", () => {
    expect(extractEpisode("Show.EP07.mkv", bracketCleanup("Show.EP07.mkv"))?.episode).toBe(7);
    expect(extractEpisode("Show.E12.mkv", bracketCleanup("Show.E12.mkv"))?.episode).toBe(12);
  });

  it("第N话 catches Chinese marker", () => {
    expect(extractEpisode("Show 第04话.ass", bracketCleanup("Show 第04话.ass"))?.episode).toBe(4);
  });
});

describe("extractEpisode — should NOT match", () => {
  it("returns null for filenames without recognized episode markers", () => {
    expect(extractEpisode("RandomFile.mkv", bracketCleanup("RandomFile.mkv"))).toBeNull();
  });

  it("does not mistake 1080P resolution tag for an episode", () => {
    // [1080P] has 'P' suffix so Pattern B's `\d+\]` doesn't capture.
    const name = "[Group][Show][1080P][.ass";
    const ep = extractEpisode(name, bracketCleanup(name));
    // Engine may still match ` 1080P` via different regex paths; the
    // important guarantee is that "1080" alone (the pixel height) is
    // not the chosen episode for a normal sample with both resolution
    // AND a real episode tag — that's covered by the named samples
    // above. Here we just assert it doesn't claim 1080.
    if (ep !== null) {
      expect(ep.episode).not.toBe(1080);
    }
  });
});

describe("extractSeason", () => {
  it("'2nd Season' → 2", () => {
    const name = "Show 2nd Season - 03.mkv";
    expect(extractSeason(name, bracketCleanup(name))).toBe(2);
  });

  it("'Season 3' → 3", () => {
    const name = "Show Season 3 - 03.mkv";
    expect(extractSeason(name, bracketCleanup(name))).toBe(3);
  });

  it("'第二季' → 2 (Chinese numeral)", () => {
    const name = "节目 第二季 - 03.mkv";
    expect(extractSeason(name, bracketCleanup(name))).toBe(2);
  });

  it("'第十二季' → 12 (compound Chinese numeral)", () => {
    const name = "节目 第十二季 - 03.mkv";
    expect(extractSeason(name, bracketCleanup(name))).toBe(12);
  });

  it("standalone S2 → 2", () => {
    const name = "Show S2 - 24.mkv";
    expect(extractSeason(name, bracketCleanup(name))).toBe(2);
  });

  it("S01E01 does NOT contribute to standalone season scan", () => {
    // Episode extractor takes care of season for S\dE\d. The standalone
    // scanner must skip "S01" when "E\d" follows so we don't double-count.
    const name = "Show.S01E05.mkv";
    // Note: extractSeason called in isolation here. The standalone S\d
    // regex has a negative lookahead for E\d so it won't match S01E05.
    // The other patterns won't match either. So default is 1.
    expect(extractSeason(name, bracketCleanup(name))).toBe(1);
  });

  it("returns 1 when no season cue", () => {
    expect(extractSeason("Show - 03.mkv", "Show - 03")).toBe(1);
  });
});

describe("parseFilename — end-to-end (season, episode)", () => {
  it("LoliHouse Isekai Nonbiri Nouka 2 - 03 → (2, 3)", () => {
    const p = parse(
      "[LoliHouse] Isekai Nonbiri Nouka 2 - 03 [WebRip 1080p HEVC-10bit AAC SRTx2].mkv"
    );
    expect(p.episode).toBe(3);
    // "Isekai Nonbiri Nouka 2" — the bare digit 2 isn't picked up by
    // any season pattern (we don't want to false-match every "X 2"),
    // so this reports season=1. Acceptable: Pattern A doesn't carry
    // season info for that style; if user has cross-season episodes
    // they'll surface as duplicates and resolve via manual edit in 5c.
    expect(p.season).toBe(1);
  });

  it("LoliHouse Tensei ... 2nd Season - 24 → (2, 24)", () => {
    const p = parse(
      "[LoliHouse] Tensei shitara Dainana Ouji Datta node, Kimama ni Majutsu wo Kiwamemasu 2nd Season - 24 [WebRip 1080p HEVC-10bit AAC SRTx2].mkv"
    );
    expect(p.episode).toBe(24);
    expect(p.season).toBe(2);
  });

  it("樱桃花字幕组 第二季 Dainanaoji S2 - 24 → (2, 24)", () => {
    const p = parse(
      "[樱桃花字幕组]转生为第七王子，随心所欲的魔法学习之路 第二季 Dainanaoji S2 - 24 [1080p][简日内嵌].mp4"
    );
    expect(p.episode).toBe(24);
    expect(p.season).toBe(2);
  });

  it("Airota [Kamiina Botan][03] → (1, 3)", () => {
    const p = parse(
      "[Airota][Kamiina Botan, Yoeru Sugata wa Yuri no Hana][03][1080p AVC AAC][CHT].mp4"
    );
    expect(p.episode).toBe(3);
    expect(p.season).toBe(1);
  });

  it("DBD-Raws [Isekai Nonbiri Nouka][01] → (1, 1)", () => {
    const p = parse("[DBD-Raws][Isekai Nonbiri Nouka][01][1080P][BDRip][HEVC-10bit][FLAC].sc.ass");
    expect(p.episode).toBe(1);
    expect(p.season).toBe(1);
  });

  it("S01E05 — both from regex match, season carried", () => {
    const p = parse("Show.S01E05.1080p.WEB-DL.mkv");
    expect(p.episode).toBe(5);
    expect(p.season).toBe(1);
  });
});

describe("buildPairings — common shapes", () => {
  it("1 video + 2 subs (multi-language) → 1 row, first sub selected", () => {
    const v = parse("[Group][Show][01][1080p].mkv");
    const s1 = parse("[Group][Show][01][1080p].sc.ass");
    const s2 = parse("[Group][Show][01][1080p].tc.ass");
    const rows = buildPairings([v], [s1, s2]);
    // Video-centric: ONE row per video. Other lang subs stay in
    // the input pool, reachable via the UI dropdown.
    expect(rows.length).toBe(1);
    expect(rows[0].video?.path).toBe(v.path);
    expect(rows[0].subtitle?.path).toBe(s1.path);
    expect(rows[0].selected).toBe(true);
    expect(rows[0].source).toBe("regex");
  });

  it("orphan video — 1 row, not selected, source=unmatched", () => {
    const v = parse("[Group][Show][01][1080p].mkv");
    const rows = buildPairings([v], []);
    expect(rows.length).toBe(1);
    expect(rows[0].video?.path).toBe(v.path);
    expect(rows[0].subtitle).toBeNull();
    expect(rows[0].source).toBe("unmatched");
    expect(rows[0].selected).toBe(false);
  });

  it("subs without a paired video produce no rows (stays in input pool)", () => {
    // Video-centric: an orphan subtitle isn't given its own row.
    // The user is looking for a sub for a video, not the other way
    // around. The sub stays available via every row's dropdown.
    const s1 = parse("[Group][Show][01][1080p].sc.ass");
    const s2 = parse("[Group][Show][02][1080p].sc.ass");
    const rows = buildPairings([], [s1, s2]);
    expect(rows.length).toBe(0);
  });

  it("rows are sorted by (season, episode)", () => {
    const v3 = parse("[G][Show][03][1080p].mkv");
    const v1 = parse("[G][Show][01][1080p].mkv");
    const v2 = parse("[G][Show][02][1080p].mkv");
    const rows = buildPairings([v3, v1, v2], []);
    expect(rows[0].video?.path).toBe(v1.path);
    expect(rows[1].video?.path).toBe(v2.path);
    expect(rows[2].video?.path).toBe(v3.path);
  });

  it("ambiguous (2 videos + 2 subs at same key) → 2 warning rows, index-paired", () => {
    const v1 = parse("[G1][Show][01][1080p].mkv");
    const v2 = parse("[G2][Show][01][1080p].mkv");
    const s1 = parse("[G1][Show][01][1080p].sc.ass");
    const s2 = parse("[G2][Show][01][1080p].sc.ass");
    const rows = buildPairings([v1, v2], [s1, s2]);
    expect(rows.length).toBe(2);
    expect(rows[0].source).toBe("warning");
    expect(rows[0].video?.path).toBe(v1.path);
    expect(rows[0].subtitle?.path).toBe(s1.path);
    expect(rows[1].source).toBe("warning");
    expect(rows[1].video?.path).toBe(v2.path);
    expect(rows[1].subtitle?.path).toBe(s2.path);
  });

  it("videos without paired subtitles all show as unmatched", () => {
    const matched = parse("[G][Show][01][1080p].mkv");
    const random = parse("README.mkv");
    const rows = buildPairings([matched, random], []);
    expect(rows.length).toBe(2);
    expect(rows[0].source).toBe("unmatched");
    expect(rows[0].video?.path).toBe(matched.path);
    expect(rows[1].source).toBe("unmatched");
    expect(rows[1].video?.path).toBe(random.path);
    expect(rows[1].key).toBe("unmatched");
  });
});

describe("deriveRenameOutputPath — exact basename match (no lang suffix)", () => {
  // Output basename equals the video basename verbatim. Lang tokens
  // like `.sc` / `.tc` / `.zh` in the source filename are stripped so
  // the player loads the sub by exact-name match.
  const dir = "C:\\foo\\";
  const expected = `${dir}[DBD-Raws][Isekai Nonbiri Nouka][01][1080P][BDRip][HEVC-10bit][FLAC].ass`;
  const video = `${dir}[DBD-Raws][Isekai Nonbiri Nouka][01][1080P][BDRip][HEVC-10bit][FLAC].mkv`;
  const subSc = `${dir}[DBD-Raws][Isekai Nonbiri Nouka][01][1080P][BDRip][HEVC-10bit][FLAC].sc.ass`;
  const subTc = `${dir}[DBD-Raws][Isekai Nonbiri Nouka][01][1080P][BDRip][HEVC-10bit][FLAC].tc.ass`;

  it("DBD-Raws .sc.ass → strips lang, output uses video basename", () => {
    expect(deriveRenameOutputPath(video, subSc, "copy_to_video", null)).toBe(expected);
  });

  it("DBD-Raws .tc.ass → strips lang, output uses video basename", () => {
    expect(deriveRenameOutputPath(video, subTc, "copy_to_video", null)).toBe(expected);
  });

  it("rename mode → target dir is the subtitle's dir, basename matches video", () => {
    expect(deriveRenameOutputPath(video, subSc, "rename", null)).toBe(expected);
  });

  it("copy_to_chosen → target dir is the chosen directory", () => {
    const chosen = "D:\\out";
    const out = deriveRenameOutputPath(video, subSc, "copy_to_chosen", chosen);
    expect(out).toBe(
      `D:\\out\\[DBD-Raws][Isekai Nonbiri Nouka][01][1080P][BDRip][HEVC-10bit][FLAC].ass`
    );
  });

  it("preserves subtitle's own extension (.srt → .srt)", () => {
    const subSrt = `${dir}EP01.zh.srt`;
    const v = `${dir}MyShow.S01E01.mkv`;
    expect(deriveRenameOutputPath(v, subSrt, "copy_to_video", null)).toBe(
      `${dir}MyShow.S01E01.srt`
    );
  });

  it("running rename twice on already-renamed sub is a no-op", () => {
    // First run produced `expected`. Pretending the user re-runs after,
    // the source is now `expected`; a second derivation lands on the
    // same path → genuine no-op the orchestration should detect.
    const out2 = deriveRenameOutputPath(video, expected, "copy_to_video", null);
    expect(out2).toBe(expected);
    expect(isNoOpRename(expected, out2)).toBe(true);
  });
});

describe("assignSubtitleToRow — manual edit", () => {
  function row(id: string, videoPath: string, subPath: string | null, selected = true): PairingRow {
    return {
      id,
      video: { path: videoPath, name: videoPath.split("/").pop() ?? "" },
      subtitle: subPath ? { path: subPath, name: subPath.split("/").pop() ?? "" } : null,
      source: subPath ? "regex" : "unmatched",
      selected,
      key: subPath ? "1|1" : "unmatched",
    };
  }
  const sub = (path: string) => ({ path, name: path.split("/").pop() ?? "" });

  it("assigns a sub to an orphan-video row", () => {
    const rows = [row("a", "/v02.mkv", null)];
    const out = assignSubtitleToRow(rows, "a", sub("/s02.ass"));
    expect(out).toHaveLength(1);
    expect(out[0].subtitle?.path).toBe("/s02.ass");
    expect(out[0].source).toBe("manual");
  });

  it("swaps subs between rows — target gets the new sub, source row becomes (video, null)", () => {
    const rows = [row("a", "/v01.mkv", "/s01.ass"), row("b", "/v02.mkv", "/s02.ass")];
    const out = assignSubtitleToRow(rows, "b", sub("/s01.ass"));
    expect(out).toHaveLength(2);
    expect(out.find((r) => r.id === "b")?.subtitle?.path).toBe("/s01.ass");
    expect(out.find((r) => r.id === "b")?.source).toBe("manual");
    // Row "a" loses its sub — it's now uniquely owned by row "b".
    expect(out.find((r) => r.id === "a")?.subtitle).toBeNull();
    expect(out.find((r) => r.id === "a")?.source).toBe("manual");
  });

  it("clears a row's subtitle when sub is null", () => {
    const rows = [row("a", "/v01.mkv", "/s01.ass")];
    const out = assignSubtitleToRow(rows, "a", null);
    expect(out).toHaveLength(1);
    expect(out[0].subtitle).toBeNull();
    expect(out[0].source).toBe("manual");
  });

  it("picking the row's current subtitle is a no-op", () => {
    const rows = [row("a", "/v01.mkv", "/s01.ass")];
    const out = assignSubtitleToRow(rows, "a", sub("/s01.ass"));
    expect(out).toBe(rows);
  });

  it("picking null on an already-null row is a no-op", () => {
    const rows = [row("a", "/v01.mkv", null)];
    const out = assignSubtitleToRow(rows, "a", null);
    expect(out).toBe(rows);
  });

  it("target row id that doesn't exist → no-op", () => {
    const rows = [row("a", "/v01.mkv", "/s01.ass")];
    const out = assignSubtitleToRow(rows, "ghost", sub("/s99.ass"));
    expect(out).toBe(rows);
  });

  it("assigning a sub never paired in any row works (sub came from input pool)", () => {
    // Common case in the new model: the sub was an "orphan" that
    // didn't get its own row. Caller looked it up in availableSubtitles
    // and passed it in. No previous row owned it, so no row is
    // unpaired as a side effect.
    const rows = [row("a", "/v01.mkv", "/s01.ass")];
    const out = assignSubtitleToRow(rows, "a", sub("/sX.ass"));
    expect(out).toHaveLength(1);
    expect(out[0].subtitle?.path).toBe("/sX.ass");
    expect(out[0].source).toBe("manual");
  });

  it("round-trip: pick sub2, then pick sub1 back → original assignments restored (manual badges remain)", () => {
    const rows = [row("a", "/v01.mkv", "/s01.ass"), row("b", "/v02.mkv", "/s02.ass")];
    const step1 = assignSubtitleToRow(rows, "b", sub("/s01.ass"));
    // After step1: a=(v01, null), b=(v02, s01)
    const step2 = assignSubtitleToRow(step1, "a", sub("/s02.ass"));
    expect(step2.find((r) => r.id === "a")?.subtitle?.path).toBe("/s02.ass");
    expect(step2.find((r) => r.id === "b")?.subtitle?.path).toBe("/s01.ass");
    expect(step2.every((r) => r.source === "manual")).toBe(true);
  });
});

describe("isNoOpRename", () => {
  it("returns true for identical paths", () => {
    const p = "C:\\foo\\bar.ass";
    expect(isNoOpRename(p, p)).toBe(true);
  });

  it("returns true when paths differ only in slash style", () => {
    expect(isNoOpRename("C:\\foo\\bar.ass", "C:/foo/bar.ass")).toBe(true);
  });

  it("returns true when paths differ only in case (Windows)", () => {
    expect(isNoOpRename("C:\\Foo\\Bar.ASS", "c:\\foo\\bar.ass")).toBe(true);
  });

  it("returns false for paths with different filenames", () => {
    expect(isNoOpRename("C:\\foo\\bar.ass", "C:\\foo\\baz.ass")).toBe(false);
  });

  it("returns false for paths in different directories", () => {
    expect(isNoOpRename("C:\\foo\\bar.ass", "C:\\bar\\bar.ass")).toBe(false);
  });

  it("treats NFC- and NFD-equivalent CJK paths as equal", () => {
    // NFC vs NFD normalization difference (composed vs decomposed).
    // Path equality must ignore the form so a sub with a decomposed
    // OS-supplied path doesn't false-positive against the NFC target.
    const nfc = "C:\\foo\\é.ass".normalize("NFC");
    const nfd = "C:\\foo\\é.ass".normalize("NFD");
    expect(isNoOpRename(nfc, nfd)).toBe(true);
  });
});

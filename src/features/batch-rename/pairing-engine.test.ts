/**
 * Pairing-engine tests.
 *
 * Anchored to representative real-world fan-sub naming patterns:
 *   - Pattern A: bracket-group prefix + ` - NN [` episode marker
 *     (single group, joint releases, bilingual CJK + romaji titles,
 *      season-suffix variants)
 *   - Pattern B: adjacent-bracket `][NN][` episode marker
 *     (single group, raw-pack style with multi-language sub variants)
 *
 * The original Western-TV regex set (S\dE\d / EP\d / 第N话) hit ZERO
 * of these samples; this suite locks in coverage so a future
 * "simplification" can't quietly regress the fan-sub paths.
 */
import { describe, it, expect } from "vitest";
import { isWindowsRuntime } from "../../lib/platform";
import {
  parseFilename,
  bracketCleanup,
  extractEpisode,
  extractSeason,
  buildPairings,
  buildMultiSubtitlePairings,
  deriveRenameOutputPath,
  findDuplicateRenameOutputKeys,
  findRenameInputConflictIndexes,
  findRenameOutputOwnershipConflictIndexes,
  countUnpairedSubtitlePaths,
  refreshRenameFilesAfterSuccessfulInPlaceRenames,
  refreshPairingRowsAfterSuccessfulInPlaceRenames,
  summarizeRenameRun,
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
  it("Pattern A — SubA Show Two 2 - 03", () => {
    const name = "[SubA] Show Two 2 - 03 [WebRip 1080p HEVC-10bit AAC SRTx2].mkv";
    const ep = extractEpisode(name, bracketCleanup(name));
    expect(ep?.episode).toBe(3);
  });

  it("Pattern A — SubA Long Sample Title 2nd Season - 24", () => {
    const name = "[SubA] Long Sample Title 2nd Season - 24 [WebRip 1080p HEVC-10bit AAC SRTx2].mkv";
    const ep = extractEpisode(name, bracketCleanup(name));
    expect(ep?.episode).toBe(24);
  });

  it("Pattern A — 字幕组Z bilingual - 24", () => {
    const name = "[字幕组Z]中文示例标题 第二季 RomajiTitle S2 - 24 [1080p][简日内嵌].mp4";
    const ep = extractEpisode(name, bracketCleanup(name));
    expect(ep?.episode).toBe(24);
  });

  it("Pattern A — SubB Sample Show - 02", () => {
    const name = "[SubB] Sample Show Title - 02 [WebRip][HEVC-10bit 1080p][CHI_JPN].mkv";
    const ep = extractEpisode(name, bracketCleanup(name));
    expect(ep?.episode).toBe(2);
  });

  it("Pattern A — episode 00 (pilot / OVA zero) is preserved", () => {
    // The Pattern A coverage previously skipped the `episode 0` edge —
    // OVA pilots and " - 00 [" prologue numbering are real-world
    // fan-sub conventions. A regex regression that excluded zero
    // (`\d+` → `[1-9]\d*`) would have left every prior at-limit pin
    // green; this test pins it directly.
    const name = "[SubA] Show - 00 [WebRip 1080p].mkv";
    const ep = extractEpisode(name, bracketCleanup(name));
    expect(ep?.episode).toBe(0);
  });

  it("Pattern A — SubC&SubA Sample Show - 03", () => {
    const name = "[SubC&SubA] Sample Show Title - 03 [WebRip 1080p HEVC-10bit AAC ASSx2].mkv";
    const ep = extractEpisode(name, bracketCleanup(name));
    expect(ep?.episode).toBe(3);
  });

  it("Pattern A — accepts a known language suffix before the subtitle extension", () => {
    const name = "Show - 01.sc.ass";
    const ep = extractEpisode(name, bracketCleanup(name));
    expect(ep?.episode).toBe(1);
  });

  it("Pattern A — rejects an unknown dotted suffix instead of treating it as a language tag", () => {
    const name = "Show - 01.commentary.ass";
    expect(extractEpisode(name, bracketCleanup(name))).toBeNull();
  });

  it("Pattern B — SubD Sample Show [03]", () => {
    const name = "[SubD][Sample Show Title][03][1080p AVC AAC][CHT].mp4";
    const ep = extractEpisode(name, bracketCleanup(name));
    expect(ep?.episode).toBe(3);
  });

  it("Pattern B — SubC Sample Show [03]", () => {
    const name = "[SubC][Sample Show Title][03][1080p][JPTC].mp4";
    const ep = extractEpisode(name, bracketCleanup(name));
    expect(ep?.episode).toBe(3);
  });

  it("Pattern B — RawsX Show Title [01]", () => {
    const name = "[RawsX][Show Title][01][1080P][BDRip][HEVC-10bit][FLAC].sc.ass";
    const ep = extractEpisode(name, bracketCleanup(name));
    expect(ep?.episode).toBe(1);
  });

  it("Pattern B — skips a four-digit year and accepts the later revised episode", () => {
    const name = "[Group][2024v2][Show Title][01v2][1080p].mkv";
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

  it("S0E05 preserves season 0 for specials", () => {
    const name = "Show.S0E05.Special.mkv";
    const ep = extractEpisode(name, bracketCleanup(name));
    expect(ep?.episode).toBe(5);
    expect(ep?.seasonFromMatch).toBe(0);
  });

  it("EP01 / E01 catches Western-ish naming (bounded to 1-3 digits)", () => {
    expect(extractEpisode("Show.EP07.mkv", bracketCleanup("Show.EP07.mkv"))?.episode).toBe(7);
    expect(extractEpisode("Show.E12.mkv", bracketCleanup("Show.E12.mkv"))?.episode).toBe(12);
    // At-limit: 3 digits still match (real episode counts top out ~999).
    expect(extractEpisode("Show EP999.mkv", bracketCleanup("Show EP999.mkv"))?.episode).toBe(999);
  });

  it("第N话 catches Chinese marker", () => {
    expect(extractEpisode("Show 第04话.ass", bracketCleanup("Show 第04话.ass"))?.episode).toBe(4);
  });

  it("accepts revision tails across every episode grammar", () => {
    const cases: [string, number][] = [
      ["Show.S01E05v2.1080p.mkv", 5],
      ["[Group][Show][06v3][1080p].mkv", 6],
      ["[Group] Show - 07V4 [1080p].mkv", 7],
      ["Show - 08v5.ass", 8],
      ["Show 第09v6话.ass", 9],
      ["Show.EP10v7.mkv", 10],
    ];

    for (const [name, episode] of cases) {
      expect(extractEpisode(name, bracketCleanup(name))?.episode, name).toBe(episode);
    }
  });
});

describe("extractEpisode — should NOT match", () => {
  it("returns null for a filename without any recognized episode marker", () => {
    expect(extractEpisode("RandomFile.mkv", bracketCleanup("RandomFile.mkv"))).toBeNull();
  });

  it("never returns 1080 as the episode (resolution-tag rejection)", () => {
    // [1080P] has 'P' suffix so Pattern B's `\d+\]` doesn't capture.
    // The engine may still match ` 1080P` via other regex paths; the
    // load-bearing guarantee is that "1080" is never reported as the
    // episode number, regardless of whether the engine returns null
    // or some other (legitimate) episode picked from the input.
    //
    // The previous form (`expect(ep?.episode).not.toBe(1080)`) passed
    // vacuously when extractEpisode returned null. The stronger form
    // below asserts BOTH branches of "valid output": either we
    // returned null (no match), or we returned a number that isn't
    // 1080.
    const name = "[Group][Show][1080P][.ass";
    const ep = extractEpisode(name, bracketCleanup(name));
    if (ep !== null) {
      expect(typeof ep.episode).toBe("number");
      expect(ep.episode).not.toBe(1080);
    }
  });

  it("over-limit EP run (4+ digits) does not prefix-truncate to a wrong episode", () => {
    // `\bEP?(\d{1,3})\b` bounds the Western EP fallback to 1-3 digits; the
    // trailing \b makes EP1234 FAIL to match rather than truncate to 123.
    // With no other marker present, the episode is unresolved (null) and the
    // file falls through to the LCS fallback instead of pairing on a wrong
    // episode. Pairs with the at-limit EP999 case above.
    expect(extractEpisode("Show EP1234.mkv", bracketCleanup("Show EP1234.mkv"))).toBeNull();
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

  it("malformed 第N季 numerals fall back to default season 1", () => {
    // Doubled 十, an overlong run, and a mixed digit+numeral are all rejected
    // by the numeral validator → default season 1, instead of the old
    // 十十→20 / 二十三四→23 wrong-but-finite keys (N-cn-numerals).
    for (const cn of ["十十", "二十三四", "1十"]) {
      const name = `节目 第${cn}季 - 03.mkv`;
      expect(extractSeason(name, bracketCleanup(name))).toBe(1);
    }
  });

  it("standalone S2 → 2", () => {
    const name = "Show S2 - 24.mkv";
    expect(extractSeason(name, bracketCleanup(name))).toBe(2);
  });

  it("standalone S0 → 0 for specials", () => {
    const name = "Show S0 - 05.mkv";
    expect(extractSeason(name, bracketCleanup(name))).toBe(0);
  });

  it("'第0季' → 0 for specials", () => {
    const name = "节目 第0季 - 05.mkv";
    expect(extractSeason(name, bracketCleanup(name))).toBe(0);
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
  it("SubA Show Two 2 - 03 → (2, 3)", () => {
    const p = parse("[SubA] Show Two 2 - 03 [WebRip 1080p HEVC-10bit AAC SRTx2].mkv");
    expect(p.episode).toBe(3);
    // "Show Two 2" — the bare digit 2 isn't picked up by
    // any season pattern (we don't want to false-match every "X 2"),
    // so this reports season=1. Acceptable: Pattern A doesn't carry
    // season info for that style; if user has cross-season episodes
    // they'll surface as duplicates and resolve via manual edit.
    expect(p.season).toBe(1);
  });

  it("SubA Long Sample Title 2nd Season - 24 → (2, 24)", () => {
    const p = parse(
      "[SubA] Long Sample Title 2nd Season - 24 [WebRip 1080p HEVC-10bit AAC SRTx2].mkv"
    );
    expect(p.episode).toBe(24);
    expect(p.season).toBe(2);
  });

  it("字幕组Z 第二季 RomajiTitle S2 - 24 → (2, 24)", () => {
    const p = parse("[字幕组Z]中文示例标题 第二季 RomajiTitle S2 - 24 [1080p][简日内嵌].mp4");
    expect(p.episode).toBe(24);
    expect(p.season).toBe(2);
  });

  it("SubD [Sample Show][03] → (1, 3)", () => {
    const p = parse("[SubD][Sample Show Title][03][1080p AVC AAC][CHT].mp4");
    expect(p.episode).toBe(3);
    expect(p.season).toBe(1);
  });

  it("RawsX [Show Title][01] → (1, 1)", () => {
    const p = parse("[RawsX][Show Title][01][1080P][BDRip][HEVC-10bit][FLAC].sc.ass");
    expect(p.episode).toBe(1);
    expect(p.season).toBe(1);
  });

  it("S01E05 — both from regex match, season carried", () => {
    const p = parse("Show.S01E05.1080p.WEB-DL.mkv");
    expect(p.episode).toBe(5);
    expect(p.season).toBe(1);
  });

  it("S0E05 — season 0 survives end-to-end", () => {
    const p = parse("Show.S0E05.Special.mkv");
    expect(p.episode).toBe(5);
    expect(p.season).toBe(0);
  });
});

describe("buildPairings — common shapes", () => {
  it("pairs a mixed batch of language-suffixed and revision-tailed names", () => {
    const videos = [parse("Show - 01v2 [1080p].mkv"), parse("Show.S01E02v3.1080p.mkv")];
    const subtitles = [parse("Show - 01.sc.ass"), parse("Show.S01E02v1.eng.ass")];
    const rows = buildPairings(videos, subtitles);

    expect(rows).toHaveLength(2);
    expect(rows.map((row) => row.subtitle?.path)).toEqual([subtitles[0]!.path, subtitles[1]!.path]);
    expect(rows.map((row) => row.source)).toEqual(["regex", "regex"]);
  });

  it("1 video + 2 subs (multi-language) → 1 row, first sub selected", () => {
    const v = parse("[Group][Show][01][1080p].mkv");
    const s1 = parse("[Group][Show][01][1080p].sc.ass");
    const s2 = parse("[Group][Show][01][1080p].tc.ass");
    const rows = buildPairings([v], [s1, s2]);
    // Video-centric: ONE row per video. Other lang subs stay in
    // the input pool, reachable via the UI dropdown.
    expect(rows.length).toBe(1);
    expect(rows[0]!.video?.path).toBe(v.path);
    expect(rows[0]!.subtitle?.path).toBe(s1.path);
    expect(rows[0]!.selected).toBe(true);
    expect(rows[0]!.source).toBe("regex");
  });

  it("orphan video — 1 row, not selected, source=unmatched", () => {
    const v = parse("[Group][Show][01][1080p].mkv");
    const rows = buildPairings([v], []);
    expect(rows.length).toBe(1);
    expect(rows[0]!.video?.path).toBe(v.path);
    expect(rows[0]!.subtitle).toBeNull();
    expect(rows[0]!.source).toBe("unmatched");
    expect(rows[0]!.selected).toBe(false);
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
    // Anchor row count first so a regression that drops a row surfaces
    // as "expected 3, got 2" rather than "expected /v01.mkv, got
    // undefined" against an out-of-bounds rows[2]!.
    expect(rows.length).toBe(3);
    expect(rows[0]!.video?.path).toBe(v1.path);
    expect(rows[1]!.video?.path).toBe(v2.path);
    expect(rows[2]!.video?.path).toBe(v3.path);
  });

  it("ambiguous (2 videos + 2 subs at same key) → 2 warning rows, index-paired", () => {
    const v1 = parse("[G1][Show][01][1080p].mkv");
    const v2 = parse("[G2][Show][01][1080p].mkv");
    const s1 = parse("[G1][Show][01][1080p].sc.ass");
    const s2 = parse("[G2][Show][01][1080p].sc.ass");
    const rows = buildPairings([v1, v2], [s1, s2]);
    expect(rows.length).toBe(2);
    expect(rows[0]!.source).toBe("warning");
    expect(rows[0]!.video?.path).toBe(v1.path);
    expect(rows[0]!.subtitle?.path).toBe(s1.path);
    expect(rows[1]!.source).toBe("warning");
    expect(rows[1]!.video?.path).toBe(v2.path);
    expect(rows[1]!.subtitle?.path).toBe(s2.path);
    // pin `selected` too. A warning row
    // has a real (debatable) pairing, so it defaults to selected=true
    // — buildPairings's `selected: sub !== null` line. The earlier
    // test didn't pin this; a regression flipping warning rows to
    // selected=false would have left ambiguous pairings unchecked by
    // default, silently dropping them from a "run all" batch.
    expect(rows[0]!.selected).toBe(true);
    expect(rows[1]!.selected).toBe(true);
  });

  it("videos without paired subtitles all show as unmatched", () => {
    const episodeNamed = parse("[G][Show][01][1080p].mkv");
    const nonEpisodeName = parse("README.mkv");
    const rows = buildPairings([episodeNamed, nonEpisodeName], []);
    expect(rows.length).toBe(2);
    expect(rows[0]!.source).toBe("unmatched");
    expect(rows[0]!.video?.path).toBe(episodeNamed.path);
    expect(rows[1]!.source).toBe("unmatched");
    expect(rows[1]!.video?.path).toBe(nonEpisodeName.path);
    expect(rows[1]!.key).toBe("unmatched");
  });

  it("ambiguous overflow (3 videos + 2 subs) → vs[2] sources unmatched AND keys unmatched", () => {
    // Boundary pin for the overflow branch in buildPairings: when
    // the ambiguous bucket has more videos than subs, the overflow
    // videos (vs[i] with i >= ss.length) fall to source="unmatched".
    // The row's `key` must also flip to "unmatched" — without it the
    // overflow row carries its real `<season>|<episode>` key but
    // sources as "unmatched", and downstream sort groups it by
    // episode instead of clustering at the tail with the other
    // unmatched entries. The equal-count test above (2v+2s) only
    // exercises the index-pair branch; this one specifically pins
    // the source/key-flip pair.
    const v1 = parse("[G1][Show][01][1080p].mkv");
    const v2 = parse("[G2][Show][01][1080p].mkv");
    const v3 = parse("[G3][Show][01][1080p].mkv");
    const s1 = parse("[G1][Show][01][1080p].sc.ass");
    const s2 = parse("[G2][Show][01][1080p].sc.ass");
    const rows = buildPairings([v1, v2, v3], [s1, s2]);
    expect(rows.length).toBe(3);
    // First two videos pair off via the ambiguous-warning path.
    expect(rows[0]!.source).toBe("warning");
    expect(rows[0]!.subtitle?.path).toBe(s1.path);
    expect(rows[1]!.source).toBe("warning");
    expect(rows[1]!.subtitle?.path).toBe(s2.path);
    // Third video gets the overflow treatment — both source AND key
    // flipped to "unmatched" so the sort clusters it at the tail.
    expect(rows[2]!.source).toBe("unmatched");
    expect(rows[2]!.subtitle).toBeNull();
    expect(rows[2]!.key).toBe("unmatched");
  });
});

describe("buildMultiSubtitlePairings — opt-in multi-subtitle GUI mode", () => {
  it("1 video + 3 language subs produces 3 selected rows for that video", () => {
    const v = parse("[RawsX][Show][01][1080p].mkv");
    const s1 = parse("[SubsA][Show][01][1080p].sc.ass");
    const s2 = parse("[SubsA][Show][01][1080p].tc.ass");
    const s3 = parse("[SubsA][Show][01][1080p].jp.srt");
    const rows = buildMultiSubtitlePairings([v], [s1, s2, s3]);

    expect(rows).toHaveLength(3);
    expect(rows.map((r) => r.video?.path)).toEqual([v.path, v.path, v.path]);
    expect(rows.map((r) => r.subtitle?.path)).toEqual([s1.path, s2.path, s3.path]);
    expect(rows.map((r) => r.selected)).toEqual([true, true, true]);
    expect(rows.map((r) => r.source)).toEqual(["regex", "regex", "regex"]);
  });

  it("ambiguous video keys still pair by index instead of creating a cross-product", () => {
    const v1 = parse("[RawA][Show][01][1080p].mkv");
    const v2 = parse("[RawB][Show][01][1080p].mkv");
    const s1 = parse("[SubsA][Show][01][1080p].sc.ass");
    const s2 = parse("[SubsB][Show][01][1080p].tc.ass");
    const rows = buildMultiSubtitlePairings([v1, v2], [s1, s2]);

    expect(rows).toHaveLength(2);
    expect(rows.map((r) => r.source)).toEqual(["warning", "warning"]);
    expect(rows.map((r) => r.video?.path)).toEqual([v1.path, v2.path]);
    expect(rows.map((r) => r.subtitle?.path)).toEqual([s1.path, s2.path]);
  });

  it("keeps unmatched videos as unselected rows", () => {
    const v = parse("Show.without.episode.mkv");
    const rows = buildMultiSubtitlePairings([v], []);

    expect(rows).toHaveLength(1);
    expect(rows[0]!.video?.path).toBe(v.path);
    expect(rows[0]!.subtitle).toBeNull();
    expect(rows[0]!.selected).toBe(false);
    expect(rows[0]!.source).toBe("unmatched");
  });
});

describe("findDuplicateRenameOutputKeys", () => {
  it("allows distinct language suffix outputs", () => {
    const duplicates = findDuplicateRenameOutputKeys([
      "C:\\Subs\\Episode.sc.ass",
      "C:\\Subs\\Episode.tc.ass",
    ]);
    expect(duplicates.size).toBe(0);
  });

  it("flags duplicate outputs after canonical alias resolution", () => {
    const duplicates = findDuplicateRenameOutputKeys([
      "C:/Subs/Episode.sc.ass",
      "C:/Subs/Episode.sc.ass",
    ]);
    expect(duplicates.size).toBe(1);
  });
});

describe("findRenameInputConflictIndexes", () => {
  it("marks both rows in a two-file swap", () => {
    const conflicts = findRenameInputConflictIndexes([
      { inputPath: "C:\\subs\\720.ass", outputPath: "C:\\subs\\1080.ass" },
      { inputPath: "C:\\subs\\1080.ass", outputPath: "C:\\subs\\720.ass" },
    ]);

    expect(Array.from(conflicts).sort()).toEqual([0, 1]);
  });

  it("marks every participant in a longer output-to-input chain", () => {
    const conflicts = findRenameInputConflictIndexes([
      { inputPath: "C:\\subs\\a.ass", outputPath: "C:\\subs\\b.ass" },
      { inputPath: "C:\\subs\\b.ass", outputPath: "C:\\subs\\c.ass" },
      { inputPath: "C:\\subs\\c.ass", outputPath: "C:\\subs\\final.ass" },
      { inputPath: "C:\\subs\\safe.ass", outputPath: "C:\\subs\\safe-out.ass" },
    ]);

    expect(Array.from(conflicts).sort()).toEqual([0, 1, 2]);
  });

  it("uses canonical slash and case aliases on Windows", () => {
    if (!isWindowsRuntime) return;
    const conflicts = findRenameInputConflictIndexes([
      { inputPath: "C:\\Subs\\A.ass", outputPath: "c:/subs/B.ass" },
      { inputPath: "C:\\SUBS\\b.ass", outputPath: "C:\\Subs\\final.ass" },
    ]);

    expect(Array.from(conflicts).sort()).toEqual([0, 1]);
  });

  it("uses canonical Unicode aliases on every platform", () => {
    const conflicts = findRenameInputConflictIndexes([
      { inputPath: "/subs/Cafe\u0301.ass", outputPath: "/subs/target.ass" },
      { inputPath: "/subs/other.ass", outputPath: "/subs/Caf\u00e9.ass" },
    ]);

    expect(Array.from(conflicts).sort()).toEqual([0, 1]);
  });

  it("does not treat an ordinary self no-op as a conflict", () => {
    const conflicts = findRenameInputConflictIndexes([
      { inputPath: "/subs/ready.ass", outputPath: "/subs/ready.ass" },
      { inputPath: "C:\\subs\\other.ass", outputPath: "C:\\subs\\other-out.ass" },
    ]);

    expect(conflicts.size).toBe(0);
  });

  it("blocks a target aimed at a loaded subtitle without an executable row", () => {
    const conflicts = findRenameInputConflictIndexes(
      [{ inputPath: "C:\\subs\\paired.ass", outputPath: "C:\\subs\\unpaired.ass" }],
      ["C:\\subs\\paired.ass", "C:\\subs\\unpaired.ass"]
    );

    expect(Array.from(conflicts)).toEqual([0]);
  });
});

describe("findRenameOutputOwnershipConflictIndexes", () => {
  it("blocks only outputs owned by another tab before the filesystem loop", () => {
    const targets = [
      { inputPath: "C:/subs/one.ass", outputPath: "C:/out/free.ass" },
      { inputPath: "C:/subs/two.ass", outputPath: "C:/out/loaded.ass" },
      { inputPath: "C:/subs/three.ass", outputPath: "C:/out/also-free.ass" },
    ];

    const conflicts = findRenameOutputOwnershipConflictIndexes(
      targets,
      (outputPath) => outputPath === "C:/out/loaded.ass"
    );

    expect(Array.from(conflicts)).toEqual([1]);
  });
});

describe("summarizeRenameRun", () => {
  it("reports complete only when every planned row succeeds", () => {
    expect(summarizeRenameRun(2, 2, 0)).toEqual({ kind: "success", totalPlanned: 2 });
  });

  it("includes preflight and runtime failures in partial-run arithmetic", () => {
    expect(summarizeRenameRun(1, 1, 1)).toEqual({ kind: "partial", totalPlanned: 2 });
    expect(summarizeRenameRun(1, 2, 0)).toEqual({ kind: "partial", totalPlanned: 2 });
  });

  it("reports an all-failed batch against the full planned denominator", () => {
    expect(summarizeRenameRun(0, 2, 1)).toEqual({ kind: "error", totalPlanned: 3 });
  });
});

describe("countUnpairedSubtitlePaths", () => {
  it("counts unique loaded subtitles that have no pairing row", () => {
    const video = parse("Show - 01.mkv");
    const paired = parse("Show - 01.sc.ass");
    const hidden = parse("Bonus commentary.ass");
    const rows = buildPairings([video], [paired, hidden]);

    expect(countUnpairedSubtitlePaths([paired.path, hidden.path, hidden.path], rows)).toBe(1);
  });

  it("reports a duplicated row owner as not uniquely paired", () => {
    const video = parse("Show - 01.mkv");
    const subtitle = parse("Show - 01.sc.ass");
    const row = buildPairings([video], [subtitle])[0]!;

    expect(countUnpairedSubtitlePaths([subtitle.path], [row, { ...row, id: "duplicate" }])).toBe(1);
  });
});

describe("refreshRenameFilesAfterSuccessfulInPlaceRenames", () => {
  it("updates only successful inputs and makes the next run an honest no-op", () => {
    const state = {
      videoPaths: ["C:/video/Show - 01.mkv", "C:/video/Show - 02.mkv"],
      videoNames: ["Show - 01.mkv", "Show - 02.mkv"],
      subtitlePaths: ["C:/subs/old - 01.ass", "C:/subs/old - 02.ass", "C:/subs/unselected.ass"],
      subtitleNames: ["old - 01.ass", "old - 02.ass", "unselected.ass"],
    };
    const outputPath = "C:/subs/Show - 01.ass";

    const refreshed = refreshRenameFilesAfterSuccessfulInPlaceRenames(state, [
      { inputPath: state.subtitlePaths[0]!, outputPath },
    ])!;

    expect(refreshed.subtitlePaths).toEqual([
      outputPath,
      state.subtitlePaths[1],
      state.subtitlePaths[2],
    ]);
    expect(refreshed.subtitleNames).toEqual(["Show - 01.ass", "old - 02.ass", "unselected.ass"]);
    expect(state.subtitlePaths[0]).toBe("C:/subs/old - 01.ass");

    const secondRunRows = buildPairings(
      [parseFilename(refreshed.videoPaths[0]!, refreshed.videoNames[0]!)],
      [parseFilename(refreshed.subtitlePaths[0]!, refreshed.subtitleNames[0]!)]
    );
    const secondOutput = deriveRenameOutputPath(
      secondRunRows[0]!.video!.path,
      secondRunRows[0]!.subtitle!.path,
      "rename",
      null
    );
    expect(isNoOpRename(secondRunRows[0]!.subtitle!.path, secondOutput)).toBe(true);
  });

  it("returns the original state when no successful input is currently loaded", () => {
    const state = {
      videoPaths: ["C:/video/Show - 01.mkv", "C:/video/Show - 02.mkv"],
      videoNames: ["Show - 01.mkv", "Show - 02.mkv"],
      subtitlePaths: ["C:/subs/old - 01.ass", "C:/subs/old - 02.ass"],
      subtitleNames: ["old - 01.ass", "old - 02.ass"],
    };

    const refreshed = refreshRenameFilesAfterSuccessfulInPlaceRenames(state, [
      { inputPath: "C:/subs/already-removed.ass", outputPath: "C:/subs/new.ass" },
    ]);

    expect(refreshed).toBe(state);
  });
});

describe("refreshPairingRowsAfterSuccessfulInPlaceRenames", () => {
  it("rewrites only successful subtitles while preserving manual and checkbox intent", () => {
    const videos = [parse("Show - 01.mkv"), parse("Show - 02.mkv")];
    const subtitles = [parse("old - 01.ass"), parse("old - 02.ass")];
    const seed = buildPairings(videos, subtitles);
    const rows: PairingRow[] = [
      { ...seed[0]!, source: "manual", selected: true },
      { ...seed[1]!, source: "manual", selected: false },
    ];

    const refreshed = refreshPairingRowsAfterSuccessfulInPlaceRenames(rows, [
      { inputPath: subtitles[0]!.path, outputPath: "/dummy/Show - 01.ass" },
    ]);

    expect(refreshed[0]).toMatchObject({
      id: rows[0]!.id,
      source: "manual",
      selected: true,
      subtitle: { path: "/dummy/Show - 01.ass", name: "Show - 01.ass" },
    });
    expect(refreshed[1]).toBe(rows[1]);
    expect(refreshed[1]).toMatchObject({ source: "manual", selected: false });
  });
});

describe("deriveRenameOutputPath — exact basename match (no lang suffix)", () => {
  // Output basename equals the video basename verbatim. Lang tokens
  // like `.sc` / `.tc` / `.zh` in the source filename are stripped so
  // the player loads the sub by exact-name match.
  const dir = "C:\\foo\\";
  const expected = `${dir}[RawsX][Show Title][01][1080P][BDRip][HEVC-10bit][FLAC].ass`;
  const video = `${dir}[RawsX][Show Title][01][1080P][BDRip][HEVC-10bit][FLAC].mkv`;
  const subSc = `${dir}[RawsX][Show Title][01][1080P][BDRip][HEVC-10bit][FLAC].sc.ass`;
  const subTc = `${dir}[RawsX][Show Title][01][1080P][BDRip][HEVC-10bit][FLAC].tc.ass`;

  it("RawsX .sc.ass → strips lang, output uses video basename", () => {
    expect(deriveRenameOutputPath(video, subSc, "copy_to_video", null)).toBe(expected);
  });

  it("RawsX .tc.ass → strips lang, output uses video basename", () => {
    expect(deriveRenameOutputPath(video, subTc, "copy_to_video", null)).toBe(expected);
  });

  it("rename mode → target dir is the subtitle's dir, basename matches video", () => {
    expect(deriveRenameOutputPath(video, subSc, "rename", null)).toBe(expected);
  });

  it("uses the subtitle directory for rename and the video directory for copy", () => {
    const separatedVideo = "D:\\video\\Show.S01E01.mkv";
    const separatedSubtitle = "C:\\subs\\old-name.ass";

    expect(deriveRenameOutputPath(separatedVideo, separatedSubtitle, "rename", null)).toBe(
      "C:\\subs\\Show.S01E01.ass"
    );
    expect(deriveRenameOutputPath(separatedVideo, separatedSubtitle, "copy_to_video", null)).toBe(
      "D:\\video\\Show.S01E01.ass"
    );
  });

  it("copy_to_chosen → target dir is the chosen directory", () => {
    const chosen = "D:\\out";
    const out = deriveRenameOutputPath(video, subSc, "copy_to_chosen", chosen);
    expect(out).toBe(`D:\\out\\[RawsX][Show Title][01][1080P][BDRip][HEVC-10bit][FLAC].ass`);
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

  // Pin the self-overwrite bypass — rename mode where the subtitle
  // path already matches the video stem must NOT trip
  // assertSafeOutputPath's self-overwrite guard. The bypass at
  // pairing-engine.ts:486 is the load-bearing line; without this
  // test a refactor that drops the guard could re-introduce the
  // regression (legitimate "rename to same name" throws self-overwrite
  // error) and existing tests would still pass.
  it("rename mode no-op (sub already matches video stem) does not throw self-overwrite", () => {
    const v = "C:\\media\\MyShow.S01E01.mkv";
    const sub = "C:\\media\\MyShow.S01E01.ass";
    // mode=rename is the branch the bypass protects: without
    // pairing-engine.ts:486's `!(mode === "rename" && isNoOpRename(...))`
    // guard, deriveRenameOutputPath here would have invoked
    // assertSafeOutputPath which self-overwrite-rejects.
    // The legitimate "rename to same name" case is OK because the
    // rename loop later skips no-op rows before any I/O attempt.
    const out = deriveRenameOutputPath(v, sub, "rename", null);
    expect(out).toBe(sub);
    expect(isNoOpRename(sub, out)).toBe(true);
  });
});

describe("deriveRenameOutputPath — language-preserving multi-subtitle mode", () => {
  const dir = "C:\\foo\\";
  const video = `${dir}Show.S01E01.mkv`;

  it("inserts a canonical language suffix before the subtitle extension", () => {
    const sub = `${dir}Show.S01E01.sc.ass`;
    expect(
      deriveRenameOutputPath(video, sub, "copy_to_video", null, { languageSuffix: "SC" })
    ).toBe(`${dir}Show.S01E01.sc.ass`);
  });

  it("preserves opaque sidecar extensions while adding the language suffix", () => {
    const sub = `${dir}Show.S01E01.sc.sup`;
    expect(
      deriveRenameOutputPath(video, sub, "copy_to_video", null, { languageSuffix: "sc" })
    ).toBe(`${dir}Show.S01E01.sc.sup`);
  });

  it("treats an already language-suffixed in-place rename as a no-op", () => {
    const sub = `${dir}Show.S01E01.sc.ass`;
    expect(deriveRenameOutputPath(video, sub, "rename", null, { languageSuffix: "sc" })).toBe(sub);
  });

  it("rejects unsafe language suffixes before output path validation", () => {
    const sub = `${dir}Show.S01E01.ass`;
    expect(() =>
      deriveRenameOutputPath(video, sub, "copy_to_video", null, { languageSuffix: "../sc" })
    ).toThrow(/Invalid language suffix/);
  });
});

describe("deriveRenameOutputPath — path-validator integration", () => {
  // Pin the contract that wires assertSafeOutputFilename +
  // assertSafeOutputPath into the rename derivation. Each mode picks
  // a different validator reference (rename → subtitlePath,
  // copy_to_video → videoPath, copy_to_chosen → chosenDir-synthesized
  // ref).
  it("rejects Windows reserved name as the output basename (CON.mkv → CON.ass)", () => {
    const video = "C:\\media\\CON.mkv";
    const sub = "C:\\media\\episode.ass";
    expect(() => deriveRenameOutputPath(video, sub, "copy_to_video", null)).toThrow(
      /reserved name/
    );
  });

  it("rejects copy_to_chosen with a chosenDir that resolves to empty after normalization", () => {
    const video = "C:\\media\\episode.mkv";
    const sub = "C:\\media\\episode.zh.ass";
    // "/" normalizes to "" after trailing-slash strip; an empty-after-
    // normalization guard converts that into a clear error rather than
    // silently accepting any rooted path.
    expect(() => deriveRenameOutputPath(video, sub, "copy_to_chosen", "/")).toThrow(
      /empty after normalization/
    );
  });

  it("accepts ordinary fan-sub names through the validator (negative control)", () => {
    const video = "C:\\media\\Show.S01E01.1080p.mkv";
    const sub = "C:\\media\\Show.S01E01.1080p.zh.ass";
    expect(() => deriveRenameOutputPath(video, sub, "copy_to_video", null)).not.toThrow();
  });

  it("accepts brace characters in video-derived output names", () => {
    // BatchRename's outName is built from the user's verbatim video
    // filename + the subtitle's extension — there's no template
    // substitution in play, so brace literals are legitimate (NTFS /
    // ext4 / APFS all allow them, and fan-sub releases routinely use
    // `[Group] Show {1080p}.mkv` style). An earlier version of this
    // case threw `illegal characters` because the shared
    // assertSafeOutputFilename gate (designed for HDR / Shift / Embed /
    // chain template resolvers, which DO need brace-literal rejection
    // so typo'd `{Format}` tokens surface as errors) was applied
    // uniformly. The gate is now split via `{ allowBraces: true }` for
    // this callsite. Other illegal-char gates (control / NTFS-reserved /
    // separators) still apply — covered by the sibling tests.
    const video = "C:\\media\\Show.{name}.mkv";
    const sub = "C:\\media\\episode.ass";
    expect(() => deriveRenameOutputPath(video, sub, "copy_to_video", null)).not.toThrow();
  });

  it("still rejects control characters in video-derived output names", () => {
    // Counter-pin to the brace-accept relaxation: only `{}` are
    // relaxed; the C0/DEL/C1 + NTFS-reserved punctuation rejects
    // continue to apply.
    const video = "C:\\media\\Show\x00name.mkv";
    const sub = "C:\\media\\episode.ass";
    expect(() => deriveRenameOutputPath(video, sub, "copy_to_video", null)).toThrow(/illegal/);
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
    expect(out[0]!.subtitle?.path).toBe("/s02.ass");
    expect(out[0]!.source).toBe("manual");
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
    // Unpaired row preserves its prior selected state — the docstring
    // promises "selected... is preserved as the user's prior intent".
    // Both rows defaulted to selected=true, so row "a" stays true.
    expect(out.find((r) => r.id === "a")?.selected).toBe(true);
  });

  it("unpair branch preserves the source row's selected=false too", () => {
    // Parallel to the swap test, but row "a" starts unticked. The docstring
    // promises preserve-as-is on the unpaired row regardless of the prior
    // value, so row "a" must stay false after losing its sub.
    const rows = [row("a", "/v01.mkv", "/s01.ass", false), row("b", "/v02.mkv", "/s02.ass", true)];
    const out = assignSubtitleToRow(rows, "b", sub("/s01.ass"));
    expect(out.find((r) => r.id === "a")?.subtitle).toBeNull();
    expect(out.find((r) => r.id === "a")?.selected).toBe(false);
  });

  it("clears a row's subtitle when sub is null", () => {
    const rows = [row("a", "/v01.mkv", "/s01.ass")];
    const out = assignSubtitleToRow(rows, "a", null);
    expect(out).toHaveLength(1);
    expect(out[0]!.subtitle).toBeNull();
    expect(out[0]!.source).toBe("manual");
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
    expect(out[0]!.subtitle?.path).toBe("/sX.ass");
    expect(out[0]!.source).toBe("manual");
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

  it("auto-ticks the row when assigning a sub to an unselected orphan", () => {
    // The user's pain point: a video with no auto-paired sub starts
    // unticked, and forgetting to tick it after manual selection silently
    // drops it from the rename batch. Picking a sub from the dropdown
    // is itself "yes, include this row".
    const rows = [row("a", "/v02.mkv", null, false)];
    const out = assignSubtitleToRow(rows, "a", sub("/s02.ass"));
    expect(out[0]!.selected).toBe(true);
    expect(out[0]!.subtitle?.path).toBe("/s02.ass");
  });

  it("preserves selected=true when reassigning a sub on an already-selected row", () => {
    const rows = [row("a", "/v01.mkv", "/s01.ass", true)];
    const out = assignSubtitleToRow(rows, "a", sub("/sX.ass"));
    expect(out[0]!.selected).toBe(true);
    expect(out[0]!.subtitle?.path).toBe("/sX.ass");
  });

  it("auto-ticks when assigning over a previously unticked, sub-bearing row", () => {
    // User had auto-paired row, unticked it, then changed their mind
    // and picked a different sub — the manual sub pick re-arms intent.
    const rows = [row("a", "/v01.mkv", "/s01.ass", false)];
    const out = assignSubtitleToRow(rows, "a", sub("/sX.ass"));
    expect(out[0]!.selected).toBe(true);
  });

  it("clearing (sub=null) preserves the row's prior selected state", () => {
    // No auto-flip on clear. A null-sub row is already a no-op in the
    // rename loop, so the prior tick is a stale-but-harmless signal
    // that gets re-armed if the user picks a new sub later.
    const rowsTrue = [row("a", "/v01.mkv", "/s01.ass", true)];
    expect(assignSubtitleToRow(rowsTrue, "a", null)[0]!.selected).toBe(true);
    const rowsFalse = [row("a", "/v01.mkv", "/s01.ass", false)];
    expect(assignSubtitleToRow(rowsFalse, "a", null)[0]!.selected).toBe(false);
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

describe("buildPairings — regression guards", () => {
  it("ambiguous videos with no subtitles → source: 'unmatched', not 'warning'", () => {
    // Two videos with identical episode keys (would normally be ambiguous)
    // but ZERO subtitles — no decision to mark "debatable". An earlier
    // form returned source: "warning", confusingly yellow-flagging a row
    // where nothing existed to argue about.
    const videos = [
      parseFilename("/v/[A] Show - 03.mkv", "[A] Show - 03.mkv"),
      parseFilename("/v/[A] Show - 03.mp4", "[A] Show - 03.mp4"),
    ];
    const rows = buildPairings(videos, []);
    expect(rows.length).toBe(2);
    for (const r of rows) {
      expect(r.source).toBe("unmatched");
      expect(r.subtitle).toBeNull();
    }
  });

  it("leading-dot subtitle filename produces output with extension preserved", () => {
    // Crafted fan-sub pack: `.ass` filename (leading-dot Unix hidden
    // convention). An earlier `subDot > 0` guard produced `<videoBase>`
    // with NO extension — no player loads the result. The `>= 0` form
    // keeps the full `.ass` as the extension.
    const out = deriveRenameOutputPath("C:/v/Show.mkv", "C:/v/.ass", "rename", null);
    expect(out.endsWith(".ass")).toBe(true);
    expect(out).not.toBe("C:/v/Show");
  });
});

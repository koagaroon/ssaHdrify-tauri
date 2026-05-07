import { describe, expect, it } from "vitest";

import { applyFontEmbed, planFontEmbed, planRename } from "./cli-engine-entry";

const SIMPLE_ASS = `[Script Info]
Title: CLI Font Test

[V4+ Styles]
Format: Name, Fontname, Fontsize, Bold, Italic
Style: Default,Arial,20,0,0

[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:00.00,0:00:01.00,Default,Hello
`;

describe("planRename", () => {
  const video = "C:\\media\\[RawsX][Show Title][01][1080P][BDRip].mkv";
  const subSc = "C:\\media\\[RawsX][Show Title][01][1080P][BDRip].sc.ass";
  const subTc = "C:\\media\\[RawsX][Show Title][01][1080P][BDRip].tc.ass";
  const subJp = "C:\\media\\[RawsX][Show Title][01][1080P][BDRip].jp.srt";

  it("auto mode plans the GUI-style first selected pairing", () => {
    const plan = planRename({
      paths: [video, subSc, subTc],
      mode: "copy_to_video",
      langs: "auto",
    });

    expect(plan.videoCount).toBe(1);
    expect(plan.subtitleCount).toBe(2);
    expect(plan.pairings).toHaveLength(1);
    expect(plan.pairings[0]).toMatchObject({
      inputPath: subSc,
      outputPath: "C:\\media\\[RawsX][Show Title][01][1080P][BDRip].ass",
      videoPath: video,
      language: "sc",
      noOp: false,
    });
  });

  it("filters explicit language aliases before planning", () => {
    const plan = planRename({
      paths: [video, subSc, subTc, subJp],
      mode: "copy_to_video",
      langs: "zh-CN,jpn",
    });

    expect(plan.pairings.map((row) => row.inputPath)).toEqual([subSc, subJp]);
    expect(plan.pairings.map((row) => row.language)).toEqual(["sc", "jp"]);
  });

  it("all mode can plan multiple language rows for one unambiguous video", () => {
    const plan = planRename({
      paths: [video, subSc, subJp],
      mode: "copy_to_video",
      langs: "all",
    });

    expect(plan.pairings).toHaveLength(2);
    expect(plan.pairings.map((row) => row.outputPath)).toEqual([
      "C:\\media\\[RawsX][Show Title][01][1080P][BDRip].ass",
      "C:\\media\\[RawsX][Show Title][01][1080P][BDRip].srt",
    ]);
  });

  it("supports copy-to-chosen output directories", () => {
    const plan = planRename({
      paths: [video, subSc],
      mode: "copy_to_chosen",
      outputDir: "D:\\out",
      langs: "auto",
    });

    expect(plan.pairings[0].outputPath).toBe("D:\\out\\[RawsX][Show Title][01][1080P][BDRip].ass");
  });

  it("marks already matched subtitles as no-op", () => {
    const alreadyMatched = "C:\\media\\[RawsX][Show Title][01][1080P][BDRip].ass";
    const plan = planRename({
      paths: [video, alreadyMatched],
      mode: "copy_to_video",
      langs: "auto",
    });

    expect(plan.pairings[0].noOp).toBe(true);
  });
});

describe("font embed engine helpers", () => {
  it("plans font usage with embed output naming", () => {
    const plan = planFontEmbed({
      inputPath: "C:\\subs\\episode.ass",
      content: SIMPLE_ASS,
      outputTemplate: "{name}.embed.ass",
    });

    expect(plan.outputPath).toBe("C:\\subs\\episode.embed.ass");
    expect(plan.fonts).toHaveLength(1);
    expect(plan.fonts[0]).toMatchObject({
      family: "Arial",
      bold: false,
      italic: false,
      fontName: "arial.ttf",
    });
    expect(plan.fonts[0].codepoints).toEqual([72, 101, 108, 111]);
  });

  it("applies uuencoded font entries before the events section", () => {
    const result = applyFontEmbed({
      content: SIMPLE_ASS,
      fonts: [{ fontName: "arial.ttf", data: [0, 1, 2] }],
    });

    expect(result.embeddedCount).toBe(1);
    expect(result.content.indexOf("[Fonts]")).toBeLessThan(result.content.indexOf("[Events]"));
    expect(result.content).toContain("fontname: arial.ttf");
  });
});

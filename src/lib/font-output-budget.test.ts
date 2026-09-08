import { describe, expect, it, vi } from "vitest";
import { applyFontEmbed } from "../cli-engine-entry";
import { runChain } from "../features/chain/chain-runtime";

vi.mock("../features/font-embed/ass-uuencode", async (importOriginal) => {
  const original = await importOriginal<typeof import("../features/font-embed/ass-uuencode")>();
  return {
    ...original,
    FontSectionBuilder: class extends original.FontSectionBuilder {
      constructor() {
        super(100);
      }
    },
  };
});

const content = "[Script Info]\nTitle: Budget test\n[Events]\n";
const font = { fontName: "sample.ttf", dataB64: "AQIDBAUGBwgJCgsMDQ4PEBESExQ=" };
const adapters = {
  standalone: (fonts: (typeof font)[]) => applyFontEmbed({ content, fonts }).content,
  chain: (fonts: (typeof font)[]) =>
    runChain({
      content,
      inputPath: "C:\\subtitles\\sample.ass",
      plan: {
        outputTemplate: "{name}.embedded.ass",
        steps: [
          {
            kind: "embed",
            params: {
              fontDirs: [],
              recursiveFontDirs: [],
              fontFiles: [],
              noSystemFonts: true,
              onMissing: "fail",
              subsets: fonts,
            },
          },
        ],
      },
    }).content,
};

describe.each(Object.entries(adapters))("%s embedded font output budget", (_name, embed) => {
  it("embeds a valid subset below the aggregate budget", () => {
    expect(embed([font])).toContain("fontname: sample.ttf\n");
  });

  it("rejects cumulative output before decoding subsequent entries", () => {
    expect(() => embed([font, font, { fontName: "later.ttf", dataB64: "invalid!" }])).toThrow(
      "100-byte encoded output limit"
    );
  });
});

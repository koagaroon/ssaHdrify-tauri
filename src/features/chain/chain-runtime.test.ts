/**
 * Chain runtime tests — pin the contract that `runChain` produces
 * the same output as manual sequential calls to the underlying
 * transforms. If a future refactor drifts the chain executor's call
 * sequence (different argument shape, missing intermediate, double-
 * application), these tests fail before any user sees it.
 *
 * Embed-step tests cover the pre-resolved-subset contract: missing
 * params throws, empty subsets array short-circuits, and a populated
 * subsets array inserts a [Fonts] section with the supplied bytes.
 */

import { describe, expect, it } from "vitest";

import { runChain, resolveChainOutputPath } from "./chain-runtime";
import type { ChainPlan } from "./chain-types";
import { processAssContent } from "../hdr-convert/ass-processor";
import { shiftSubtitles } from "../timing-shift/timing-engine";

// ── Fixtures ───────────────────────────────────────────────

const ASS_FIXTURE = [
  "[Script Info]",
  "ScriptType: v4.00+",
  "",
  "[V4+ Styles]",
  "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding",
  "Style: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,2,2,10,10,10,1",
  "",
  "[Events]",
  "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text",
  "Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,Hello world",
  "Dialogue: 0,0:00:04.00,0:00:06.00,Default,,0,0,0,,Second line",
  "",
].join("\n");

const INPUT_PATH = "C:\\subs\\episode01.ass";

// ── HDR-only chain ──────────────────────────────────────────

describe("runChain — single HDR step", () => {
  it("produces same content as direct processAssContent call", () => {
    const plan: ChainPlan = {
      steps: [{ kind: "hdr", params: { eotf: "PQ", brightness: 1000 } }],
      outputTemplate: "{name}.hdr.ass",
    };
    const result = runChain({ plan, inputPath: INPUT_PATH, content: ASS_FIXTURE });
    const expected = processAssContent(ASS_FIXTURE, 1000, "PQ");
    expect(result.content).toBe(expected);
  });

  it("HLG variant matches direct call", () => {
    const plan: ChainPlan = {
      steps: [{ kind: "hdr", params: { eotf: "HLG", brightness: 4000 } }],
      outputTemplate: "{name}.hdr.ass",
    };
    const result = runChain({ plan, inputPath: INPUT_PATH, content: ASS_FIXTURE });
    const expected = processAssContent(ASS_FIXTURE, 4000, "HLG");
    expect(result.content).toBe(expected);
  });

  it("emits no notes for a single HDR step", () => {
    const plan: ChainPlan = {
      steps: [{ kind: "hdr", params: { eotf: "PQ", brightness: 203 } }],
      outputTemplate: "{name}.hdr.ass",
    };
    const result = runChain({ plan, inputPath: INPUT_PATH, content: ASS_FIXTURE });
    expect(result.notes).toEqual([]);
  });
});

// ── Shift-only chain ────────────────────────────────────────

describe("runChain — single shift step", () => {
  it("produces same content as direct shiftSubtitles call", () => {
    const plan: ChainPlan = {
      steps: [{ kind: "shift", params: { offsetMs: 2000 } }],
      outputTemplate: "{name}.shifted.ass",
    };
    const result = runChain({ plan, inputPath: INPUT_PATH, content: ASS_FIXTURE });
    const expected = shiftSubtitles(ASS_FIXTURE, { offsetMs: 2000 });
    expect(result.content).toBe(expected.content);
  });

  it("threshold variant matches direct call", () => {
    const plan: ChainPlan = {
      steps: [{ kind: "shift", params: { offsetMs: -500, thresholdMs: 60000 } }],
      outputTemplate: "{name}.shifted.ass",
    };
    const result = runChain({ plan, inputPath: INPUT_PATH, content: ASS_FIXTURE });
    const expected = shiftSubtitles(ASS_FIXTURE, {
      offsetMs: -500,
      thresholdMs: 60000,
    });
    expect(result.content).toBe(expected.content);
  });

  it("emits a diagnostic note with shift counts and detected format", () => {
    const plan: ChainPlan = {
      steps: [{ kind: "shift", params: { offsetMs: 2000 } }],
      outputTemplate: "{name}.shifted.ass",
    };
    const result = runChain({ plan, inputPath: INPUT_PATH, content: ASS_FIXTURE });
    expect(result.notes).toHaveLength(1);
    // `format` value is lowercase per `SubtitleFormat` (e.g., "ass") —
    // case-insensitive match so the test doesn't pin the format-token
    // casing to a particular convention.
    expect(result.notes[0]).toMatch(/shift: \d+\/\d+ entries shifted \(format: ass\)/i);
  });
});

// ── Multi-step chains: HDR + Shift in both orders ───────────

describe("runChain — HDR + Shift composition", () => {
  it("hdr then shift matches manual sequential calls", () => {
    const plan: ChainPlan = {
      steps: [
        { kind: "hdr", params: { eotf: "PQ", brightness: 1000 } },
        { kind: "shift", params: { offsetMs: 2000 } },
      ],
      outputTemplate: "{name}.hdr.shifted.ass",
    };
    const result = runChain({ plan, inputPath: INPUT_PATH, content: ASS_FIXTURE });

    const afterHdr = processAssContent(ASS_FIXTURE, 1000, "PQ");
    const afterShift = shiftSubtitles(afterHdr, { offsetMs: 2000 });
    expect(result.content).toBe(afterShift.content);
  });

  it("shift then hdr matches manual sequential calls (different order, different result)", () => {
    const plan: ChainPlan = {
      steps: [
        { kind: "shift", params: { offsetMs: 2000 } },
        { kind: "hdr", params: { eotf: "PQ", brightness: 1000 } },
      ],
      outputTemplate: "{name}.shifted.hdr.ass",
    };
    const result = runChain({ plan, inputPath: INPUT_PATH, content: ASS_FIXTURE });

    const afterShift = shiftSubtitles(ASS_FIXTURE, { offsetMs: 2000 });
    const afterHdr = processAssContent(afterShift.content, 1000, "PQ");
    expect(result.content).toBe(afterHdr);
  });

  it("collects notes from each step in order", () => {
    const plan: ChainPlan = {
      steps: [
        { kind: "hdr", params: { eotf: "PQ", brightness: 1000 } },
        { kind: "shift", params: { offsetMs: 2000 } },
      ],
      outputTemplate: "{name}.hdr.shifted.ass",
    };
    const result = runChain({ plan, inputPath: INPUT_PATH, content: ASS_FIXTURE });
    // HDR currently emits no notes; shift emits one. If HDR adds
    // notes later, this assertion's shape stays — just the count
    // changes.
    expect(result.notes).toHaveLength(1);
    expect(result.notes[0]).toMatch(/shift:/);
  });
});

// ── Embed step (pre-resolved subsets contract) ──────────────

describe("runChain — embed step", () => {
  it("throws a clear error when params.subsets is undefined", () => {
    // Rust shell didn't pre-resolve — defensive error pointing at
    // the contract violation rather than crashing in applyFontEmbed.
    const plan: ChainPlan = {
      steps: [
        {
          kind: "embed",
          params: {
            fontDirs: [],
            fontFiles: [],
            noSystemFonts: false,
            onMissing: "warn",
            // subsets intentionally omitted
          },
        },
      ],
      outputTemplate: "{name}.embed.ass",
    };
    expect(() => runChain({ plan, inputPath: INPUT_PATH, content: ASS_FIXTURE })).toThrow(
      /step 1 \(embed\) failed: embed step in chain requires pre-resolved font subsets/
    );
  });

  it("returns input content unchanged when subsets array is empty", () => {
    // Legit case: subtitle has no font references, or all lookups
    // failed under --on-missing warn. Skip the [Fonts] insertion.
    const plan: ChainPlan = {
      steps: [
        {
          kind: "embed",
          params: {
            fontDirs: [],
            fontFiles: [],
            noSystemFonts: false,
            onMissing: "warn",
            subsets: [],
          },
        },
      ],
      outputTemplate: "{name}.embed.ass",
    };
    const result = runChain({ plan, inputPath: INPUT_PATH, content: ASS_FIXTURE });
    expect(result.content).toBe(ASS_FIXTURE);
    expect(result.notes).toEqual(["embed: 0 fonts embedded (no resolvable references)"]);
  });

  it("inserts a [Fonts] section when subsets are provided", () => {
    // Synthetic single-byte payload — the test verifies the section
    // appears, not the UU-encoded contents. buildFontEntry is itself
    // tested in ass-uuencode.test.ts.
    const plan: ChainPlan = {
      steps: [
        {
          kind: "embed",
          params: {
            fontDirs: [],
            fontFiles: [],
            noSystemFonts: false,
            onMissing: "warn",
            // base64("\x00\x01\x02\x03") === "AAECAw==". Decoded by
            // chain-runtime via js-base64 → matches the Rust shell's
            // serde-base64 wire format. (js-base64, not atob: the CLI
            // runtime is bare deno_core without Web APIs.)
            subsets: [{ fontName: "Arial.ttf", dataB64: "AAECAw==" }],
          },
        },
      ],
      outputTemplate: "{name}.embed.ass",
    };
    const result = runChain({ plan, inputPath: INPUT_PATH, content: ASS_FIXTURE });
    expect(result.content).toContain("[Fonts]");
    expect(result.content).toContain("fontname: Arial.ttf");
    expect(result.notes).toEqual(["embed: 1 font(s) embedded"]);
  });
});

// ── Output path resolution ──────────────────────────────────

describe("resolveChainOutputPath", () => {
  it("substitutes {name} and {ext} from the input path", () => {
    expect(resolveChainOutputPath("C:\\subs\\episode01.ass", "{name}.hdr.shifted.ass")).toBe(
      "C:\\subs\\episode01.hdr.shifted.ass"
    );
  });

  it("preserves backslash style on Windows-shape inputs", () => {
    expect(resolveChainOutputPath("C:\\subs\\ep01.ass", "{name}.processed.ass")).toBe(
      "C:\\subs\\ep01.processed.ass"
    );
  });

  it("preserves forward-slash style on POSIX-shape inputs", () => {
    expect(resolveChainOutputPath("/home/u/ep01.ass", "{name}.processed.ass")).toBe(
      "/home/u/ep01.processed.ass"
    );
  });

  it("accepts drive-root input (regression for Z:\\file.ass)", () => {
    // Same regression class as the bug fixed in commit d01402b — the
    // shared decomposeInputPath helper guarantees drive-root is
    // accepted consistently here too.
    expect(resolveChainOutputPath("Z:\\cat.ass", "{name}.hdr.shifted.embed.ass")).toBe(
      "Z:\\cat.hdr.shifted.embed.ass"
    );
  });

  it("substitutes {ext} preserving the dot", () => {
    expect(resolveChainOutputPath("C:\\subs\\ep01.ass", "{name}.shifted{ext}")).toBe(
      "C:\\subs\\ep01.shifted.ass"
    );
  });

  it("collapses adjacent dots from empty token substitutions", () => {
    // A template like `{name}.{lang}.ass` with no `{lang}` token
    // support at chain level leaves `{lang}` literal — but if a user
    // writes a deliberate `{name}..ass`, substituteTemplate normalizes
    // it (template-literal dot-run collapse, Phase A).
    expect(resolveChainOutputPath("C:\\subs\\ep01.ass", "{name}..processed.ass")).toBe(
      "C:\\subs\\ep01.processed.ass"
    );
  });

  it("preserves intentional `..` inside user content (Round 1 F4.N-R1-1)", () => {
    // Fan-sub filenames legitimately carry `..` in the baseName — e.g.,
    // `[Group]Show..special.ass`. The blanket post-substitution
    // collapse used previously fused those into a single dot, corrupting
    // the output filename. The boundary-aware substituteTemplate now
    // touches dots only at value↔literal junctions and inside template
    // literals; value-internal `..` survives.
    expect(
      resolveChainOutputPath("C:\\subs\\[Group]Show..special.ass", "{name}.shifted{ext}")
    ).toBe("C:\\subs\\[Group]Show..special.shifted.ass");
    expect(
      resolveChainOutputPath("C:\\subs\\[Group]Show..special.ass", "{name}.shifted.{ext}")
    ).toBe("C:\\subs\\[Group]Show..special.shifted.ass");
  });

  it("substitutes values containing `$` literally (Round 1 F4.A-R1-3)", () => {
    // String#replace with $-bearing replacement strings interprets
    // `$&` / `$'` / `` $` `` / `$<N>` as backref tokens. Filenames
    // with `$` are rare but legal (Windows admin shares like `c$`,
    // batch scripts named `build_$1.ass`, etc.). substituteTemplate
    // routes values through split-join so `$&` stays literal.
    expect(resolveChainOutputPath("C:\\subs\\build_$&_v2.ass", "{name}.shifted{ext}")).toBe(
      "C:\\subs\\build_$&_v2.shifted.ass"
    );
  });

  it("rejects relative-path inputs (must be absolute)", () => {
    expect(() => resolveChainOutputPath("ep01.ass", "{name}.processed.ass")).toThrow(
      /must be absolute/
    );
  });

  it("rejects unsafe output filenames (e.g., reserved names)", () => {
    expect(() => resolveChainOutputPath("C:\\subs\\CON.ass", "{name}.processed.ass")).toThrow(
      /reserved name/
    );
  });

  // ── Round 8 Wave 8.3 — chain template + shape pins ──

  it("rejects unknown template tokens (chain-level allowlist)", () => {
    // N-R8-N1-2: chain-level templates support only {name} / {ext}.
    // Per-step tokens like {eotf} / {format} previously substituted
    // to "" silently; now they throw at resolve time so users see the
    // mismatch instead of getting a path with collapsed dots.
    expect(() => resolveChainOutputPath(INPUT_PATH, "{name}.{eotf}.ass")).toThrow(/unknown token/);
    expect(() => resolveChainOutputPath(INPUT_PATH, "{name}.{format}.ass")).toThrow(
      /unknown token/
    );
  });

  it("hdr step rejects non-ASS input with an actionable message", () => {
    // A-R8-A1-7: feeding raw SRT to a chain HDR step previously
    // produced garbage output silently. The shape guard now throws
    // with guidance pointing at standalone `hdr` first.
    const plan: ChainPlan = {
      steps: [{ kind: "hdr", params: { eotf: "PQ", brightness: 1000 } }],
      outputTemplate: "{name}.hdr.ass",
    };
    const srt = "1\n00:00:01,000 --> 00:00:02,000\nHello\n";
    expect(() => runChain({ plan, inputPath: INPUT_PATH, content: srt })).toThrow(
      /requires ASS .* SSA content/
    );
  });
});

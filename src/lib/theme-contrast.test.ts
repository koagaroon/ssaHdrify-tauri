import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const css = readFileSync(new URL("../index.css", import.meta.url), "utf8").replace(
  /\/\*[\s\S]*?\*\//g,
  ""
);
type Theme = { palette: "cool" | "warm"; mode: "light" | "dark" };
type Tokens = Map<string, string>;

function themeTokens({ palette, mode }: Theme): Tokens {
  const rules = [...css.matchAll(/([^{}]+)\{([^{}]*)\}/g)].flatMap((match, order) => {
    const selectors = match[1]!.trim().split(",");
    return selectors.flatMap((rawSelector) => {
      const selector = rawSelector.trim();
      if (!selector.startsWith(":root")) return [];
      const attributes = [...selector.matchAll(/\[data-(palette|theme)=["']([^"']+)["']\]/g)];
      // Only root token rules apply; descendant/component rules do not redefine the theme.
      if (`:root${attributes.map((attribute) => attribute[0]).join("")}` !== selector) return [];
      if (
        attributes.some((attribute) =>
          attribute[1] === "palette" ? attribute[2] !== palette : attribute[2] !== mode
        )
      )
        return [];
      return [{ body: match[2]!, specificity: 1 + attributes.length, order }];
    });
  });
  rules.sort((a, b) => a.specificity - b.specificity || a.order - b.order);
  const tokens: Tokens = new Map();
  for (const rule of rules) {
    for (const declaration of rule.body.matchAll(/--([\w-]+)\s*:\s*([^;]+);/g)) {
      tokens.set(declaration[1]!, declaration[2]!.trim());
    }
  }
  return tokens;
}

function color(tokens: Tokens, name: string, resolving = new Set<string>()): string {
  if (resolving.has(name)) throw new Error(`Circular theme token --${name}`);
  resolving.add(name);
  const value = tokens.get(name);
  const reference = value?.match(/^var\(--([\w-]+)\)$/);
  if (reference) return color(tokens, reference[1]!, resolving);
  if (!value || !/^#(?:[0-9a-f]{3}|[0-9a-f]{6})$/i.test(value))
    throw new Error(`Expected an opaque hex color for --${name}; received ${value}`);
  const hex = value.slice(1);
  return hex.length === 3 ? [...hex].map((channel) => channel + channel).join("") : hex;
}

function luminance(hex: string): number {
  return [0.2126, 0.7152, 0.0722].reduce((sum, weight, index) => {
    const channel = parseInt(hex.slice(index * 2, index * 2 + 2), 16) / 255;
    return (
      sum + weight * (channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4)
    );
  }, 0);
}
function contrast(tokens: Tokens, foreground: string, background: string): number {
  const a = luminance(color(tokens, foreground));
  const b = luminance(color(tokens, background));
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}

const surfaces = [
  "bg-app",
  "bg-header",
  "bg-secondary",
  "bg-panel",
  "bg-input",
  "bg-hover",
  "bg-selected",
];

for (const palette of ["cool", "warm"] as const) {
  for (const mode of ["light", "dark"] as const) {
    describe(`${palette} gray / ${mode} contrast`, () => {
      const tokens = themeTokens({ palette, mode });

      it("keeps primary, secondary, and muted text readable on every neutral surface", () => {
        for (const foreground of ["text-primary", "text-secondary", "text-muted"]) {
          for (const background of surfaces) {
            expect(
              contrast(tokens, foreground, background),
              `${foreground} on ${background}`
            ).toBeGreaterThanOrEqual(4.5);
          }
        }
      });

      it("keeps action, hover, disabled, and status labels readable", () => {
        const pairs = [
          ["accent-text", "accent"],
          ["accent-text", "accent-hover"],
          ["accent-disabled-text", "accent-disabled-bg"],
          ["cancel-text", "cancel-bg"],
          ["cancel-text", "cancel-bg-hover"],
          ["badge-green-text", "badge-green-bg"],
          ["badge-red-text", "badge-red-bg"],
        ] as const;
        for (const [foreground, background] of pairs) {
          expect(
            contrast(tokens, foreground, background),
            `${foreground} on ${background}`
          ).toBeGreaterThanOrEqual(4.5);
        }
        for (const foreground of ["success", "warning", "error", "info"]) {
          for (const background of surfaces) {
            expect(
              contrast(tokens, foreground, background),
              `${foreground} on ${background}`
            ).toBeGreaterThanOrEqual(4.5);
          }
        }
      });

      it("distinguishes accent focus and selection indicators from their surrounding surfaces", () => {
        for (const background of surfaces) {
          expect(
            contrast(tokens, "accent", background),
            `accent against ${background}`
          ).toBeGreaterThanOrEqual(3);
        }
      });
    });
  }
}

it("resolves distinct app surfaces for all four palette and mode combinations", () => {
  const backgrounds = ["cool", "warm"].flatMap((palette) =>
    ["light", "dark"].map((mode) => color(themeTokens({ palette, mode } as Theme), "bg-app"))
  );
  expect(new Set(backgrounds).size).toBe(4);
});

import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const css = readFileSync(new URL("../index.css", import.meta.url), "utf8");
const darkStart = css.indexOf('[data-theme="dark"]');
const themes = { light: css.slice(css.indexOf(":root {"), darkStart), dark: css.slice(darkStart) };
function luminance(hex: string): number {
  const expanded = hex.length === 3 ? [...hex].map((c) => c + c).join("") : hex;
  return [0.2126, 0.7152, 0.0722].reduce((sum, weight, i) => {
    const channel = parseInt(expanded.slice(i * 2, i * 2 + 2), 16) / 255;
    return (
      sum + weight * (channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4)
    );
  }, 0);
}
function contrast(theme: string, foreground: string, background: string): number {
  const color = (name: string) => theme.match(new RegExp(`--${name}:\\s*#([0-9a-f]+)`))![1]!;
  const a = luminance(color(foreground)),
    b = luminance(color(background));
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}

describe("enabled text contrast", () => {
  for (const [name, theme] of Object.entries(themes)) {
    it(`${name} action labels and informative text meet normal-text contrast`, () => {
      expect(contrast(theme, "accent-text", "accent")).toBeGreaterThanOrEqual(4.5);
      for (const foreground of ["text-secondary", "text-muted"]) {
        for (const background of ["bg-app", "bg-header", "bg-panel", "bg-input"]) {
          expect(
            contrast(theme, foreground, background),
            `${foreground} on ${background}`
          ).toBeGreaterThanOrEqual(4.5);
        }
      }
    });
  }
});

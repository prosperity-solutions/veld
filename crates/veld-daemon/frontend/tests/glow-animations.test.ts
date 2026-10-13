import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { LIGHT_CSS } from "../src/feedback-overlay/styles";

const SHADOW = readFileSync(resolve(__dirname, "../src/feedback-overlay.css"), "utf8");

/** The text of a named `@keyframes` block. */
function keyframes(css: string, name: string): string {
  const start = css.indexOf(`@keyframes ${name}`);
  expect(start, `@keyframes ${name} exists`).toBeGreaterThanOrEqual(0);
  let depth = 0;
  for (let i = css.indexOf("{", start); i < css.length; i++) {
    if (css[i] === "{") depth++;
    if (css[i] === "}" && --depth === 0) return css.slice(start, i + 1);
  }
  throw new Error(`unterminated @keyframes ${name}`);
}

/**
 * The infinite glows used to keyframe `box-shadow`, which repaints the element
 * every frame for as long as it is on screen. They must stay on properties the
 * compositor animates (opacity / transform), with the shadow itself static.
 */
describe("infinite glow animations stay compositor-only", () => {
  const cases: Array<[string, string, string]> = [
    ["fab ring", SHADOW, "veld-feedback-fab-glow"],
    ["popover glow", SHADOW, "veld-feedback-popover-glow"],
    ["frozen frame glow", LIGHT_CSS, "veld-feedback-frame-glow"],
  ];
  for (const [label, css, name] of cases) {
    it(`${label} animates opacity/transform only`, () => {
      const block = keyframes(css, name);
      expect(block).not.toMatch(/box-shadow|filter|width|height|inset|top|left/);
      expect(block).toMatch(/opacity/);
    });
  }

  it("the frozen frame no longer carries the animation itself", () => {
    expect(LIGHT_CSS).not.toMatch(/screenshot-frame-show\s*\{[^}]*animation/);
  });
});

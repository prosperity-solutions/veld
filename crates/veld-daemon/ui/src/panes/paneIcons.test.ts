import { describe, expect, it } from "vitest";
// Imported as data, not read from disk: the UI's tsconfig has no Node types, and
// this must stay a plain type-checked import so a schema that stops parsing is a
// build error rather than a runtime one.
import schema from "../../../../../schema/v3/veld.schema.json";
import { PANE_ICONS } from "./paneIcons";

/**
 * The third link in the icon-name chain.
 *
 * `veld_core::ide::PANE_ICON_NAMES` is checked against the schema enum by a Rust
 * test; this checks the *renderer* against the same enum. Without it a name can
 * be accepted by the config, validated by the editor, pass `veld lint` — and
 * then render as a fallback glyph with nothing anywhere saying why.
 */
describe("pane icon names", () => {
  it("cover exactly the schema's paneIconName enum", () => {
    const enumerated: string[] = schema.$defs.paneIconName.enum;
    expect(enumerated.length).toBeGreaterThan(0);
    expect(Object.keys(PANE_ICONS).sort()).toEqual([...enumerated].sort());
  });

  it("map every name to something React can render", () => {
    for (const [name, icon] of Object.entries(PANE_ICONS)) {
      // Not `toBeTypeOf("function")`: Tabler wraps its icons in `forwardRef`,
      // which is an object. What matters is that the name resolved to anything
      // at all — a typo here imports `undefined` and renders nothing.
      expect(icon, `${name} must resolve to a component`).toBeTruthy();
    }
  });
});

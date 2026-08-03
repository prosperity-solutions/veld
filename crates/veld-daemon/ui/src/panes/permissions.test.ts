import { describe, expect, it } from "vitest";
// Imported rather than read off disk: this package has no `@types/node`, and the
// gate is the assertion, not the mechanism.
import schema from "../../../../../schema/v3/veld.schema.json";
import type { PermissionSetting } from "./browserHost";
import { PERMISSION_LABELS, effectiveLabel, permissionSentence, userChoice } from "./permissions";

const setting = (over: Partial<PermissionSetting> = {}): PermissionSetting => ({
  id: "camera",
  verdict: "ask",
  source: "default",
  ...over,
});

describe("the permission id list", () => {
  // The fourth copy of this list, and the one that had no gate: the schema is
  // asserted against `veld_core::ide::PERMISSION_IDS` (Rust) and
  // `VELD_PERMISSIONS` (desktop shell), but nothing under `ui/` read it. Adding
  // an id passed every suite while the panel silently rendered the raw id —
  // `PERMISSION_LABELS` is a `Record<PermissionId, …>`, so tsc stays green as
  // long as the *union* is updated, and the union is hand-maintained too.
  it("matches the JSON schema, label for label", () => {
    expect(Object.keys(PERMISSION_LABELS).sort()).toEqual(
      [...schema.$defs.permissionId.enum].sort(),
    );
  });

  it("gives every permission a human sentence, not an id", () => {
    for (const [id, label] of Object.entries(PERMISSION_LABELS)) {
      expect(label.title, id).not.toBe("");
      // The prompt reads "<origin> wants to <asking>", so a fragment that is just
      // the id ("wants to camera") is the dialog written for its implementer.
      expect(label.asking, id).not.toBe(id);
      expect(label.asking.length, id).toBeGreaterThan(2);
    }
  });
});

describe("permissionSentence", () => {
  it("joins the ids one Electron request can cover", () => {
    expect(permissionSentence(["camera"])).toBe("use your camera");
    expect(permissionSentence(["camera", "microphone"])).toBe(
      "use your camera and use your microphone",
    );
  });

  it("says something rather than nothing when the id list is empty", () => {
    // Reachable: an Electron version adds a permission veld does not model.
    expect(permissionSentence([])).not.toBe("");
  });
});

describe("userChoice", () => {
  // The regression this function exists for: the buttons used to show the
  // resolved verdict, which made `Default` unpressable on any permission
  // `veld.json` granted — clearing the override re-resolved the row straight back
  // to Allow and lit that button up, so the click looked refused.
  it("is `default` unless the user themselves set it", () => {
    expect(userChoice(setting({ verdict: "allow", source: "config" }))).toBe("default");
    expect(userChoice(setting({ verdict: "allow", source: "default" }))).toBe("default");
    expect(userChoice(setting({ verdict: "ask", source: "default" }))).toBe("default");
  });

  it("is the user's own verdict when they set one", () => {
    expect(userChoice(setting({ verdict: "allow", source: "user" }))).toBe("allow");
    expect(userChoice(setting({ verdict: "deny", source: "user" }))).toBe("deny");
  });
});

describe("effectiveLabel", () => {
  it("names where a non-default answer came from", () => {
    // A project's grant must never read as the user's own decision.
    expect(effectiveLabel(setting({ verdict: "allow", source: "config" }))).toContain("veld.json");
    expect(effectiveLabel(setting({ verdict: "allow", source: "user" }))).toContain("by you");
    expect(effectiveLabel(setting({ verdict: "allow", source: "default" }))).toContain(
      "Veld default",
    );
    expect(effectiveLabel(setting({ verdict: "deny", source: "config" }))).toContain("Blocked");
  });

  it("says nothing for the ordinary untouched row", () => {
    // Twenty-one rows are mostly this one; a suffix on each would be noise.
    expect(effectiveLabel(setting({ verdict: "ask", source: "default" }))).toBe("");
  });

  it("still explains an `ask` that somebody chose", () => {
    expect(effectiveLabel(setting({ verdict: "ask", source: "config" }))).toContain("Will ask");
  });
});

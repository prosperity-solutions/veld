import { describe, expect, it } from "vitest";
import type { ExtensionSpec } from "../api";
// Imported as data, not read from disk — the same reason `paneIcons.test.ts`
// does it: the UI's tsconfig has no Node types, and a schema that stops parsing
// should be a build error rather than a runtime one.
import schema from "../../../../../schema/v3/veld.schema.json";
import { MIN_POLL_MS, pollInterval, resolveExtension } from "./Extensions";

function spec(over: Partial<ExtensionSpec> = {}): ExtensionSpec {
  return {
    id: "pr",
    label: "PR",
    kind: "status",
    slot: "topBar",
    align: "start",
    available: true,
    when_missing: "hint",
    ...over,
  };
}

/**
 * `refresh_seconds`'s schema constraints — from the `status` branch, which is the
 * only one that declares them. Read through a function that throws rather than
 * with `!`, so a schema reshaped out from under this test fails by name instead of
 * comparing against `undefined`.
 */
function refreshSecondsSchema(): { minimum: number; default: number } {
  const branch = schema.$defs.extension.allOf.find(
    (arm) => arm.if.properties.type.const === "status",
  );
  if (!branch) throw new Error("no `status` branch in $defs.extension.allOf");
  const field = branch.then.properties.refresh_seconds;
  if (!field) throw new Error("the `status` branch declares no refresh_seconds");
  return { minimum: field.minimum, default: field.default };
}

/**
 * The fourth link in the interval chain.
 *
 * Rust owns the floor (`MIN_EXTENSION_REFRESH_SECONDS`) and the schema publishes it;
 * this file hand-duplicates it as `MIN_POLL_MS` so the browser does not wake more
 * often than the daemon will ever answer differently. Nothing tied the two together,
 * which is exactly the drift the repo's other cross-language constants are gated
 * against.
 */
describe("poll interval", () => {
  it("floors at the schema's own minimum refresh interval", () => {
    const { minimum } = refreshSecondsSchema();
    expect(minimum).toBeGreaterThan(0);
    expect(MIN_POLL_MS).toBe(minimum * 1000);
  });

  it("uses the schema's default when an entry declares no interval", () => {
    const fallback = refreshSecondsSchema().default;
    expect(pollInterval([spec({ refresh_seconds: undefined })])).toBe(fallback * 1000);
  });

  it("is zero when nothing needs evaluating, so no timer is armed", () => {
    // An `action` runs on a click and a `menu` runs nothing — a bar of those must
    // not wake the browser on a timer at all.
    expect(pollInterval([])).toBe(0);
    expect(pollInterval([spec({ kind: "action" }), spec({ kind: "menu" })])).toBe(0);
    // Nor should an unavailable badge: its command cannot run.
    expect(pollInterval([spec({ available: false })])).toBe(0);
  });

  it("takes the shortest declared interval, since one request covers them all", () => {
    const interval = pollInterval([
      spec({ id: "slow", refresh_seconds: 600 }),
      spec({ id: "fast", refresh_seconds: 30 }),
    ]);
    expect(interval).toBe(30_000);
  });

  it("never goes below the floor, whatever a config asked for", () => {
    // The daemon clamps `refresh_seconds` itself, so this is belt-and-braces
    // against a value that arrived from an older or hand-edited source.
    expect(pollInterval([spec({ refresh_seconds: 1 })])).toBe(MIN_POLL_MS);
  });
});

/**
 * `when_missing` is the project's answer about a tool the machine lacks, and the
 * one rule that is easy to get backwards is that it **beats** the user's
 * hide-disabled-actions preference — a project's install instruction must not be
 * silenced by a preference about clutter.
 */
describe("resolving an unavailable extension", () => {
  it("renders an available one plainly", () => {
    expect(resolveExtension(spec())).toEqual({
      spec: spec(),
      disabled: false,
      reason: undefined,
    });
  });

  it("hides one that asked to be hidden", () => {
    expect(resolveExtension(spec({ available: false, when_missing: "hide" }))).toBeNull();
  });

  it("names the missing tool when it only asked to be disabled", () => {
    const r = resolveExtension(
      spec({ available: false, when_missing: "disable", missing: ["gh", "python3"] }),
    );
    expect(r?.disabled).toBe(true);
    expect(r?.reason).toBe("Needs gh, python3");
    // Deliberately *not* carrying the hint: `disable` is the value an author picks
    // when there is nothing worth linking to.
    expect(r?.reason).not.toContain("Install");
  });

  it("carries the project's own sentence on the hint path", () => {
    const r = resolveExtension(
      spec({
        available: false,
        when_missing: "hint",
        missing: ["gh"],
        hint: { text: "Install the GitHub CLI.", href: "https://cli.github.com" },
      }),
    );
    expect(r?.disabled).toBe(true);
    expect(r?.reason).toBe("Needs gh. Install the GitHub CLI.");
  });

  it("still says something useful when the hint text is absent", () => {
    // `hint` with no `hint` object is a config that lints clean; the control must
    // not render an empty tooltip.
    const r = resolveExtension(spec({ available: false, missing: ["gh"] }));
    expect(r?.reason).toBe("Needs gh");
  });

  it("does not claim to know what is missing when the daemon did not say", () => {
    const r = resolveExtension(spec({ available: false, missing: [] }));
    expect(r?.reason).toBe("Not available on this machine");
  });
});

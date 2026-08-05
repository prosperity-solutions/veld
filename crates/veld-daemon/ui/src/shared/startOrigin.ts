// What a run was started from, rendered honestly.
//
// The problem this solves is not a missing field, it is two different questions
// rendered in the same register: the top bar's start control shows what ▶ will
// start *next* (a client-side choice, `veld.start.<path>`), and it sits beside a
// daemon-derived status dot. Nothing reconciled them, so a run started from the
// CLI — or by a coding agent — left a preset name from an old local choice next
// to a green "running" dot, and it read as "preset X is running".
//
// A run now records its own origin (`RunInfo.started_from`), so the two values
// become statements about the same thing. The catch is that a stored preset name
// is only true for an instant: presets are re-read from disk on every use, so the
// name can be renamed, deleted, or re-pointed at different nodes while the run is
// live. That is why a run records the *expansion* the name meant at start time
// too, and why nothing here prints a bare name — it is compared against the
// current expansion first, and a mismatch is stated.

import type { Preset, StartOrigin } from "../api";

/** Whether the preset a run named still means what it meant at start time. */
export type PresetState =
  /** Defined, and expands to exactly what the run recorded. */
  | "current"
  /** Still defined, but expands to a different node set now. */
  | "redefined"
  /** No longer in the config (renamed or deleted). */
  | "removed";

/**
 * Compare a run's recorded expansion with the preset's expansion today.
 *
 * Both sides are sorted `node:variant` token lists — the daemon sorts what it
 * records (`StartOrigin::new`) and what it ships per preset (`Preset.expanded`),
 * so this is a plain element-wise compare and not a set operation.
 *
 * An empty `expanded` means the preset exists but does not expand today (a
 * dangling `@ref`, a node that was removed). That compares as `redefined`, which
 * is the honest reading: whatever the name means now, it is not what ran.
 */
export function presetState(origin: StartOrigin, presets: Preset[]): PresetState {
  // Callers pass `null` when they cannot know — see `startOriginLabel`.
  const preset = presets.find((p) => p.name === origin.preset);
  if (!preset) return "removed";
  const now = preset.expanded ?? [];
  const then = origin.selections;
  if (now.length === then.length && now.every((t, i) => t === then[i])) {
    return "current";
  }
  return "redefined";
}

/**
 * One line naming what a run was started from, or `null` when the run predates
 * the record.
 *
 * `null` for the origin is deliberately not the string "unknown": a surface that
 * has nothing to say must say nothing, because "unknown" reads as a property of
 * the run rather than of the recording.
 *
 * **`presets: null` means "this surface does not know the config"** — Runs mode
 * polls `/api/environments` only, and has no preset list to compare against.
 * Without that distinction it would pass `[]` and every run would read
 * `no longer defined`, which is a confident falsehood produced by missing data:
 * the exact failure this module exists to avoid. With `null` the line states the
 * preset in the past tense and shows the tokens that actually ran, so nothing is
 * claimed about what the name means today.
 */
export function startOriginLabel(
  origin: StartOrigin | null | undefined,
  presets: Preset[] | null,
): string | null {
  if (!origin) return null;
  if (!origin.preset) {
    // An explicit-token start. Naming the tokens is the whole answer — saying
    // "no preset" would imply one was expected.
    return origin.selections.length > 0
      ? origin.selections.join(", ")
      : "no selections recorded";
  }
  if (presets === null) {
    const tokens = origin.selections.join(", ");
    return tokens
      ? `preset ${origin.preset} · ${tokens}`
      : `preset ${origin.preset}`;
  }
  switch (presetState(origin, presets)) {
    case "current":
      return `preset ${origin.preset}`;
    case "redefined":
      return `preset ${origin.preset} (redefined since start)`;
    case "removed":
      return `preset ${origin.preset} (no longer defined)`;
  }
}

/**
 * Whether a run's origin disagrees with the config as it reads now — the
 * condition a surface may want to mark rather than only spell out.
 *
 * False for an origin with no preset: an explicit-token start cannot drift,
 * because there is no name whose meaning could have moved.
 */
export function originIsStale(
  origin: StartOrigin | null | undefined,
  presets: Preset[] | null,
): boolean {
  // Unknown config is not staleness. A surface that cannot compare must not
  // mark anything — see `startOriginLabel`.
  if (presets === null) return false;
  if (!origin?.preset) return false;
  return presetState(origin, presets) !== "current";
}

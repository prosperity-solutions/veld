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
  | "removed"
  /**
   * Defined, but it does not expand — a dangling `@ref`, a since-removed node, a
   * cycle, or a tree over the expansion budget.
   *
   * Its own case rather than folded into `redefined`, because they are different
   * claims: "this name means something else now" versus "this name means nothing
   * resolvable". `veld status` says `cannot be expanded — see \`veld lint\``, and
   * two surfaces describing one config state differently is how a reader ends up
   * trusting the wrong one.
   */
  | "unexpandable"
  /**
   * Nothing is known: the caller has no config to compare against, or the daemon
   * did not expand this preset on this poll.
   *
   * Deliberately *not* `unexpandable`. That label points at `veld lint`, and
   * sending a reader to a check that passes is worse than saying nothing — missing
   * data must never be rendered as a finding about their config.
   */
  | "unknown";

/**
 * Compare a run's recorded expansion with the preset's expansion today.
 *
 * Both sides are sorted `node:variant` token lists — the daemon sorts what it
 * records (`StartOrigin::new`) and what it ships per preset (`Preset.expanded`),
 * so this is a plain element-wise compare and not a set operation.
 *
 * `expanded: null` means the preset exists but the daemon could not expand it — a
 * dangling `@ref`, a since-removed node, or a tree over its expansion budget. That
 * is `unexpandable`, deliberately not `redefined`: "means something else now" and
 * "means nothing resolvable" are different claims, and the CLI makes the second one
 * too. An empty *array* is a real expansion of a preset whose selections are empty,
 * and compares normally.
 */
export function presetState(origin: StartOrigin, presets: Preset[]): PresetState {
  // Callers pass `null` for `presets` when they cannot know — see `startOriginLabel`.
  const preset = presets.find((p) => p.name === origin.preset);
  if (!preset) return "removed";
  if (preset.expansion.state === "failed") return "unexpandable";
  // The listing declined to expand it this poll; that is our ignorance, not a
  // property of their config.
  if (preset.expansion.state !== "ok") return "unknown";
  const now = preset.expansion.tokens;
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
  // Nothing to compare against, so the line states the invocation in the past tense
  // and lets the tokens carry the truth.
  const unknown = () => {
    const tokens = origin.selections.join(", ");
    return tokens
      ? `preset ${origin.preset} · ${tokens}`
      : `preset ${origin.preset}`;
  };
  if (presets === null) return unknown();
  switch (presetState(origin, presets)) {
    case "current":
      return `preset ${origin.preset}`;
    case "redefined":
      return `preset ${origin.preset} (redefined since start)`;
    case "removed":
      return `preset ${origin.preset} (no longer defined)`;
    // Same wording as `veld status`'s `start_origin_label`, on purpose: one config
    // state must not be described two ways by two surfaces.
    case "unexpandable":
      return `preset ${origin.preset} (cannot be expanded — see \`veld lint\`)`;
    // Reached when the listing skipped this preset. Rendered exactly as an
    // unreadable config is, because it is the same thing from the reader's side:
    // we do not know.
    case "unknown":
      return unknown();
  }
}


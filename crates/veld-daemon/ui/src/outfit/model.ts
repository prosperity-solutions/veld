// outfit (outfit.oss.life.li) profiles, as the create dialog and the top bar
// offer them. Pure, so the rules that decide what is sent are tested on their
// own — see `model.test.ts`.
import type { OutfitProfile, OutfitState } from "../api";

/** The picker's value for "send no `outfit` key". A colon cannot start a
 *  profile name, so it cannot collide with one. */
export const FOLLOW_CLONE = ":follow";

/** Whether outfit is in play. Anything else renders nothing — no noise for
 *  people who do not use it. */
export function outfitReady(state: OutfitState | null): state is OutfitState {
  return state?.status === "ready";
}

/** What a profile swaps, in a sentence fit for a Select's secondary line. */
export function profileSwaps(files: readonly string[]): string {
  return files.length === 0 ? "the team's files" : `swaps ${files.join(", ")}`;
}

export interface OutfitOption {
  value: string;
  label: string;
  /** What choosing it does; rendered under the label. */
  description: string;
}

/**
 * The create dialog's options: the clone's own profile first — unless the repo
 * requires a choice — then one per profile.
 */
export function outfitOptions(state: OutfitState): OutfitOption[] {
  const profiles: OutfitOption[] = state.profiles.map((p) => ({
    value: p.name,
    label: p.name,
    description: profileSwaps(p.files),
  }));
  if (state.required) return profiles;
  // A pinned profile is not the clone's own, so naming it would mislabel the follow.
  const follow = state.current && !state.current.pinned ? state.current.name : undefined;
  return [
    {
      value: FOLLOW_CLONE,
      label: follow ? `Follow the clone's profile (${follow})` : "Follow the clone's profile",
      description: "pins nothing",
    },
    ...profiles,
  ];
}

/**
 * What the picker opens on: the repo's `default` when it is offered. Otherwise,
 * when no choice is required, "follow the clone"; when one is, the clone's
 * current profile, else the first one.
 */
export function initialOutfitChoice(state: OutfitState): string {
  const offered = (name: string | undefined) =>
    name !== undefined && state.profiles.some((p) => p.name === name) ? name : null;
  // Not required: following the clone is the choice that changes nothing, and
  // picking the clone's current profile by name would quietly *pin* the checkout to
  // it — a later clone-wide `outfit use` would then no longer reach it.
  if (!state.required) return offered(state.default) ?? FOLLOW_CLONE;
  return (
    offered(state.default) ??
    offered(state.current?.name) ??
    state.profiles[0]?.name ??
    FOLLOW_CLONE
  );
}

/** The `outfit` value for the create call, or `undefined` for "omit the key". */
export function outfitForCreate(
  state: OutfitState | null,
  choice: string | null,
): string | undefined {
  if (!outfitReady(state) || choice === null || choice === FOLLOW_CLONE) return undefined;
  return state.profiles.some((p) => p.name === choice) ? choice : undefined;
}

/** The top bar's one-word answer to "which profile is this checkout on". */
export function currentLabel(state: OutfitState): string {
  return state.current?.name ?? "outfit";
}

export function isCurrent(state: OutfitState, profile: OutfitProfile): boolean {
  return state.current?.name === profile.name;
}

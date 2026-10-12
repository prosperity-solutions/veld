/**
 * The one-time offer to move Claude Code from Veld's wrapper to Veld's mod
 * (`terminal.claudeIntegration`), made the first time a Claude session reports in.
 *
 * Asked at that moment rather than on load because it is the one moment the
 * question is about something in front of the person: a Claude session just
 * started in one of their terminals. Somebody who never runs Claude in Veld is
 * never asked.
 *
 * Whether it was asked lives in the daemon's promotion store, under an id in the
 * namespace `model.ts` reserves for sources other than Veld's own cards, so every
 * window and every browser profile agree and no new daemon state was needed.
 */

import type { SettingsDoc } from "../api";

/** Never renamed and never reused, by the promotion store's rules. */
export const CLAUDE_MOD_OFFER_ID = "offer:claude-mod";

/**
 * Whether an agent event is the one that prompts the offer: a Claude session
 * starting. Only the wrapper and the mod report `ready`, once per launch.
 */
export function isClaudeLaunch(tool: unknown, state: unknown): boolean {
  return tool === "claude" && state === "ready";
}

/**
 * Whether the offer still has something to offer this person.
 *
 * Not once they are on the mod already, and not when both agent switches are off —
 * the integration would then carry nothing either way. Not once the store has any
 * answer for it: closing the dialog counts as an answer, so it is asked exactly once.
 * `null` settings mean "not known yet", which is never a yes.
 */
export function wantsClaudeModOffer(
  settings: SettingsDoc | null,
  states: Record<string, unknown>,
): boolean {
  if (settings === null) return false;
  if (settings["terminal.claudeIntegration"] === "mod") return false;
  const hooks = settings["terminal.agentIntegration"] !== false;
  const context = settings["terminal.agentContext"] !== false;
  if (!hooks && !context) return false;
  return !(CLAUDE_MOD_OFFER_ID in states);
}

type Listener = () => void;

/**
 * Where the IDE channel says a Claude session started. A listener, not a store:
 * the decision needs settings and the promotion store, which the app has and the
 * channel does not.
 */
class ClaudeLaunches {
  private listeners = new Set<Listener>();

  notice(tool: unknown, state: unknown): void {
    if (!isClaudeLaunch(tool, state)) return;
    for (const listener of this.listeners) listener();
  }

  subscribe(listener: Listener): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }
}

export const claudeLaunches = new ClaudeLaunches();

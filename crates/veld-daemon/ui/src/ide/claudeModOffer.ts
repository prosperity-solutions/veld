/**
 * The one-time offer to move Claude Code from Veld's wrapper to Veld's mod
 * (`terminal.claudeIntegration`), made just before a pane that runs Claude starts.
 *
 * **Before, not after.** The integration is fixed when Claude launches — the
 * terminal's environment either names the mod or does not — so an offer made once a
 * session had reported in switched a setting the session in front of the person
 * could not follow, and it then went on behaving the old way. Asked first, the pane
 * starts the way the answer says. Claude typed into a plain terminal is never asked
 * about: nothing sees it coming, and Settings and `veld doctor` cover it.
 *
 * Whether it was asked lives in the daemon's promotion store, under an id in the
 * namespace `promotions/model.ts` reserves for sources other than Veld's own cards,
 * so every window and every browser profile agree and no new daemon state was needed.
 */

import type { PaneSpec, SettingsDoc } from "../api";

/** Never renamed and never reused, by the promotion store's rules. */
export const CLAUDE_MOD_OFFER_ID = "offer:claude-mod";

/**
 * Whether a pane about to start a session must ask first: one whose own `argv`
 * runs Claude (the daemon's `agent_tool`), starting a session rather than showing
 * one that is already running.
 */
export function asksBeforeStarting(spec: Pick<PaneSpec, "agent_tool"> | undefined, existed: boolean): boolean {
  return !existed && spec?.agent_tool === "claude";
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

type Asker = () => Promise<void>;

let asker: Asker | null = null;
let pending: Promise<void> | null = null;

/**
 * Install what a pane waits on: the app's function that asks if it has to and
 * resolves once answered (or at once when there is nothing to ask). Returns the
 * uninstaller.
 */
export function setClaudeModAsker(next: Asker): () => void {
  asker = next;
  return () => {
    if (asker === next) asker = null;
  };
}

/**
 * Resolves when a Claude pane may start. Every pane that asks while the question
 * is open waits on the same answer, and a failure never holds a pane back.
 */
export function askBeforeClaude(): Promise<void> {
  if (pending) return pending;
  if (!asker) return Promise.resolve();
  const asking = asker()
    .catch(() => {})
    .finally(() => {
      pending = null;
    });
  pending = asking;
  return asking;
}

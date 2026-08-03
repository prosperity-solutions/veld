/**
 * Toasts — the app's one way of reporting that something failed or landed.
 *
 * Replaces `window.alert` (runs mode) and a dismissable banner (IDE mode). An
 * alert blocks the page and steals the keyboard, which for a failed action fired
 * from a pane is worse than the failure; a banner reflows the layout under the
 * panes every time it appears.
 *
 * **`data-veld-overlay` on error toasts, and it is load-bearing.** Under Electron a
 * browser pane is a native view that paints over all DOM regardless of z-index, so
 * an error toast landing on one would be invisible — the exact failure the guard in
 * `panes/overlayGuard.ts` exists for. The attribute is that guard's opt-in, so the
 * panes freeze (on a captured still, not blank) while the toast is on screen and
 * resume when it goes. It goes on the individual notification rather than on the
 * `<Notifications />` container, which is mounted for the life of the page and
 * would hide every pane forever.
 *
 * **Confirmations deliberately do not carry it.** The suspend is all-or-nothing
 * across the window, and freezing every preview for three seconds to announce a
 * clipboard write is a bad trade. Nothing is lost in practice: a confirmation
 * follows a click in a menu or popover, and that surface is portalled — so it has
 * already suspended the panes while the toast appears under it.
 */

import { notifications } from "@mantine/notifications";

/**
 * How long a toast stays.
 *
 * **This is also the pane-freeze budget**, which is why an error is 5s and not
 * 15: `pushBrowserSuspend` is global, so a marked toast freezes *every* embedded
 * browser pane in the window — not only the ones it overlaps — for as long as it
 * is on screen (longer if Mantine's hover-pause holds it). Long enough to read a
 * daemon refusal, short enough that a frozen preview is not the memorable part.
 */
const ERROR_MS = 5000;
const INFO_MS = 3000;

/**
 * Report a failed action.
 *
 * `context` names what was being attempted, because a message from the daemon
 * ("run 'x' has no services opted into peer sharing") does not say which control
 * produced it when several are on screen.
 */
export function notifyError(context: string, error: unknown): void {
  const message = error instanceof Error ? error.message : String(error);
  notifications.show({
    color: "red",
    title: context,
    message,
    autoClose: ERROR_MS,
    "data-veld-overlay": true,
  });
}

/** Confirm something that produced no visible change of its own (a copy). */
export function notifyDone(message: string): void {
  notifications.show({
    color: "green",
    message,
    autoClose: INFO_MS,
  });
}

/**
 * Report something the app did instead of what was asked — a redirect, not a
 * failure and not a confirmation.
 *
 * Carries `data-veld-overlay` where `notifyDone` does not, and the difference is
 * the trigger, not the tone. A confirmation follows a click in a portalled menu
 * or popover, which has already suspended the panes; this one follows a click on
 * a plain control (a rail row), so nothing has suspended anything and a toast in
 * the top-right lands under whatever browser pane is there — invisible, which
 * for a message explaining why the click did something else is the whole loss.
 * Cost is the usual one: every embedded pane freezes on a still for `INFO_MS`.
 *
 * **At most one is ever on screen**, and that is what keeps the cost a constant.
 * The freeze lasts as long as *any* marked toast is rendered, so unbounded
 * toasts are an unbounded freeze: clicking four worktrees another window has —
 * or one of them four times — would otherwise stack four independent 3s timers
 * and hold every pane on a stale still for twelve seconds. Hidden and re-shown
 * rather than merely deduplicated by id, because only the newest redirect is
 * worth reading and Mantine drops a `show` whose id is already up, which would
 * leave the wrong worktree named.
 */
const REDIRECT_ID = "veld-redirect";

export function notifyRedirect(message: string): void {
  notifications.hide(REDIRECT_ID);
  notifications.show({
    id: REDIRECT_ID,
    color: "blue",
    message,
    autoClose: INFO_MS,
    "data-veld-overlay": true,
  });
}

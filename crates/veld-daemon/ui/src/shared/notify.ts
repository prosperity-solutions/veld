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

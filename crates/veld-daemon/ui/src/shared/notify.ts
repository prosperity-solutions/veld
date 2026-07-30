/**
 * Toasts — the app's one way of reporting that something failed or landed.
 *
 * Replaces `window.alert` (runs mode) and a dismissable banner (IDE mode). An
 * alert blocks the page and steals the keyboard, which for a failed action fired
 * from a pane is worse than the failure; a banner reflows the layout under the
 * panes every time it appears.
 *
 * **`data-veld-overlay` on every toast, and it is load-bearing.** Under Electron a
 * browser pane is a native view that paints over all DOM regardless of z-index, so
 * a toast landing on one would be invisible — the exact failure the guard in
 * `panes/overlayGuard.ts` exists for. The attribute is that guard's opt-in, so the
 * panes freeze (on a captured still, not blank) while a toast is on screen and
 * resume when it goes. It goes on the individual notification rather than on the
 * `<Notifications />` container, which is mounted for the life of the page and
 * would hide every pane forever.
 */

import { notifications } from "@mantine/notifications";

/** Errors stay up longer than confirmations — they carry text worth reading. */
const ERROR_MS = 8000;
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
    "data-veld-overlay": true,
  });
}

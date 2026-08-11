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

import { createElement } from "react";
import { notifications } from "@mantine/notifications";
import type { DesktopAppApi } from "../shell";

/** The Electron shell's app bridge, looked up lazily so this module stays
 *  import-safe in a node test environment (no `window`). */
function desktopAppBridge(): DesktopAppApi | null {
  return (
    (window as { veldDesktop?: { app?: DesktopAppApi } }).veldDesktop?.app ?? null
  );
}

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

/**
 * An OS-level notification (a banner) via the Web Notification API.
 *
 * The "different window or browser" half of terminal notifications: a banner
 * surfaces even when the tab or window that owns the pane is backgrounded, and
 * clicking it focuses that window (the OS does this) and then runs `onClick`,
 * which is how the pane comes up. The in-app toast is the other half, for a
 * window that is already on screen.
 *
 * One helper covers both builds: in the Electron renderer `new Notification`
 * maps to a native notification, and in a plain browser it is the standard
 * permission-gated API. Permission is requested lazily on first use and never
 * prompted again once denied — a notification a user has refused is not worth
 * re-asking for on every bell.
 */
export interface SystemNotificationOptions {
  title: string;
  body?: string;
  /** The pane that produced the notification — echoed back on click so the
   *  page can focus it. `sessionId` *is* the terminal tab id. */
  worktreeId: number;
  sessionId: string;
  /** Browser-only click handler. In Veld Desktop the click comes back through
   *  `desktopApp.onNotifyClick` instead, which the app subscribes to. */
  onClick?: () => void;
}

export function showSystemNotification(opts: SystemNotificationOptions): void {
  // Veld Desktop: the MAIN process owns the native banner, so the click can
  // reliably focus the window even when it is backgrounded. The shell echoes
  // the click back through `desktopApp.onNotifyClick` (with the worktree and
  // session echoed), and the page's click handler runs there.
  const bridge = desktopAppBridge();
  // `notify` is optional: an older shell (or a shell not yet restarted onto
  // this build) exposes the app bridge without it, and must fall back to the
  // browser path rather than throwing on a missing method.
  if (typeof bridge?.notify === "function") {
    void bridge.notify({
      title: opts.title,
      body: opts.body ?? "",
      worktreeId: opts.worktreeId,
      sessionId: opts.sessionId,
    });
    return;
  }
  // A plain browser tab: the Web Notification API, permission-gated. Clicking
  // focuses the tab (the OS does this) and runs `onClick`.
  if (typeof Notification === "undefined") return;
  const show = () => {
    try {
      // Silent for the same reason the desktop shell's banner is: the terminal
      // rings its own bell for this, at the user's `terminal.bellVolume`, and
      // the OS's default notification chime would be a second sound for one
      // event — one that a bell volume of 0 could not turn off. A browser that
      // ignores the flag falls back to its own sound, which is what it did
      // before.
      const n = new Notification(opts.title, { body: opts.body, silent: true });
      if (opts.onClick) {
        n.onclick = () => {
          // Focus the owning window/tab, then hand to the caller.
          window.focus();
          opts.onClick?.();
        };
      }
    } catch {
      // A notification that cannot be shown is not worth a toast of its own.
    }
  };
  if (Notification.permission === "granted") {
    show();
  } else if (Notification.permission === "default") {
    // Ask once. A granted answer shows the banner; a denied one is a real
    // signal the user has chosen (or the browser blocks) — say why rather than
    // silently staying quiet.
    void Notification.requestPermission().then((p) => {
      if (p === "granted") {
        show();
      } else if (p === "denied") {
        console.warn(
          "[veld] terminal notification blocked — this site is not allowed to send notifications. Click the lock in the address bar → Notifications.",
        );
      }
    });
  } else {
    console.warn(
      "[veld] terminal notification blocked — this site is not allowed to send notifications. Click the lock in the address bar → Notifications.",
    );
  }
}

/**
 * The in-app half of a terminal notification: a clickable toast naming the
 * worktree and pane, so a pane that finished while you were looking at it is
 * one click away. Clicking focuses the pane; the system banner (for a
 * backgrounded window or tab) is the other half, via [`showSystemNotification`].
 */
export interface TerminalNotifyOptions {
  title: string;
  message: string;
  onClick: () => void;
}

export function notifyTerminal(opts: TerminalNotifyOptions): void {
  notifications.show({
    title: opts.title,
    // The toast's whole surface is clickable, but that is only discoverable if
    // it says so — a visible "click to focus" hint on its own line.
    message: createElement(
      "span",
      { className: "term-notify-body" },
      opts.message,
      createElement("span", { className: "term-notify-focus" }, "Click to focus"),
    ),
    color: "teal",
    onClick: opts.onClick,
    autoClose: 8000,
  });
}

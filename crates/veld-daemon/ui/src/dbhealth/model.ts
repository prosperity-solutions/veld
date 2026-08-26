/**
 * What to say about the health of veld's own database, and how loudly.
 *
 * Deliberately thin and pure: every decision the banner makes lives here, where
 * the node-environment test suite can reach it — the same split
 * `promotions/model.ts` uses. The component below it renders what this returns
 * and owns no rules of its own.
 *
 * **Why the states are compared as strings.** The daemon's `state` fields are
 * open sets: a daemon newer than this bundle can name a fault this code has
 * never heard of, and `just dev-ui` proxies `/api` to whatever daemon is
 * *installed*. So anything that is not a known-good value is treated as a fault
 * worth showing rather than being narrowed away — the failure this whole feature
 * exists to fix was a real fault rendered as silence.
 */

import type { DbHealth } from "../api";

/** How serious the condition is, which decides how the banner is painted. */
export type Severity = "none" | "warn" | "error";

export interface Notice {
  severity: Severity;
  /** One line, the thing that is wrong. */
  headline: string;
  /** One line, what it means for the reader. */
  detail: string;
  /** Short label for the top bar, or `null` for nothing to show there. */
  marker: string | null;
  /** Whether to offer the restore action. */
  canRestore: boolean;
  /**
   * Set when a system notification has not been raised for this fault yet.
   * Claiming it is the caller's job (`api.markDbHealthNotified`).
   */
  notify: { id: string; title: string; body: string } | null;
}

export const NO_NOTICE: Notice = {
  severity: "none",
  headline: "",
  detail: "",
  marker: null,
  canRestore: false,
  notify: null,
};

/** Backup states that are not a complaint. */
const BACKUPS_FINE = new Set(["ok", "off", "unknown"]);

/**
 * Turn a health payload into the one thing worth telling the user.
 *
 * `null` in (nothing fetched yet, or an older daemon that has no such endpoint)
 * means no notice: absence of data is not evidence of damage, and a permanent
 * scary banner on every older daemon is how a banner gets ignored.
 */
export function noticeFor(health: DbHealth | null | undefined): Notice {
  if (!health) return NO_NOTICE;

  const dbState = health.database?.state ?? "ok";
  const backups = health.backups?.state ?? "unknown";
  const candidate = health.restore?.candidate ?? null;
  const notify = health.notify ?? null;

  if (dbState !== "ok") {
    return {
      severity: "error",
      headline: "Veld's database is damaged",
      // The database is veld's own state, not the user's code — say so, because
      // the first fear on reading "database" here is for the repository.
      detail: candidate
        ? `Your projects, worktrees and settings are at risk. The newest usable backup was taken ${describeAge(candidate.takenAt)}.`
        : "Your projects, worktrees and settings are at risk, and there is no usable backup to restore.",
      marker: "database damaged",
      canRestore: !!candidate,
      notify,
    };
  }

  if (!BACKUPS_FINE.has(backups)) {
    const since = health.backups?.lastOk ?? health.backups?.newest?.takenAt ?? null;
    return {
      severity: "warn",
      headline: "Veld has stopped backing up",
      detail: backupDetail(health, since),
      marker: "backups failing",
      // A restore is not the answer to "backups are failing" — the live
      // database is fine, and putting an old copy over it would *lose* state.
      canRestore: false,
      notify,
    };
  }

  return NO_NOTICE;
}

/**
 * One clean sentence about the backup schedule, plus the reason *only when there
 * is one worth adding*.
 *
 * **The daemon's `lastError` is not always a reason.** When nothing has attempted
 * a backup at all, the daemon derives that message itself from the newest file on
 * disk — so pasting it after our own sentence produced, verbatim:
 *
 * > The last successful backup was 2 hours ago. no backup has been written since
 * > 2026-08-26T09:22:00+00:00 — one was due every 60 minute(s)
 *
 * which states the age twice, drops a raw RFC3339 timestamp into user-facing
 * copy, and reads as two sentences with the second one uncapitalised.
 * `lastAttempt` is what tells the two apart: a real attempt that failed sets it,
 * the derived deadman path does not. So the reason is shown when something
 * actually tried and failed, and otherwise the interval carries the message.
 */
function backupDetail(health: DbHealth, since: string | null): string {
  const every = everyPhrase(health.backups?.intervalMinutes);
  const opening = since
    ? `The last successful backup was ${describeAge(since)}, and ${every}.`
    : `No backup has been written yet, and ${every}.`;
  // `lastAttempt` OR the state itself. The daemon only ever reports `failing`
  // when something tried and errored (`overdue` is the derived, nothing-tried
  // case), so the state carries the same fact — and keying on `lastAttempt`
  // alone would silently drop the reason if a daemon sent one without the other,
  // which is exactly the version-skew this file is careful about everywhere else.
  const attempted =
    !!health.backups?.lastAttempt || (health.backups?.state ?? "") === "failing";
  const reason = attempted ? reasonOf(health) : "";
  return reason ? `${opening} The last attempt failed: ${reason}` : opening;
}

/** "one is due every hour" / "…every 90 minutes" — the UI's own words. */
function everyPhrase(intervalMinutes: number | undefined): string {
  const minutes = intervalMinutes ?? 60;
  if (minutes === 60) return "one is due every hour";
  if (minutes % 60 === 0) return `one is due every ${minutes / 60} hours`;
  return `one is due every ${minutes} minutes`;
}

/** The daemon's own words about why, trimmed to something a banner can hold. */
function reasonOf(health: DbHealth): string {
  const error = health.backups?.lastError?.trim();
  if (!error) return "";
  const line = error.split("\n")[0].trim();
  const trimmed = line.length > 160 ? `${line.slice(0, 157)}…` : line;
  // The daemon's messages are log-shaped and start lowercase; this one lands
  // mid-sentence in a banner.
  return trimmed.charAt(0).toUpperCase() + trimmed.slice(1);
}

/**
 * "3 hours ago" — coarse on purpose. The reader's decision is between "recent
 * enough" and "far too long ago", and a precise duration invites arithmetic
 * nobody needs to do.
 *
 * Exported for the tests, and for the restore dialog, which has to say what
 * restoring would cost.
 */
export function describeAge(iso: string, now: Date = new Date()): string {
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "at an unknown time";
  const minutes = Math.floor((now.getTime() - then) / 60_000);
  if (minutes < 0) return "in the future (check the clock)";
  if (minutes < 2) return "just now";
  if (minutes < 60) return `${minutes} minutes ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return hours === 1 ? "an hour ago" : `${hours} hours ago`;
  const days = Math.floor(hours / 24);
  return days === 1 ? "yesterday" : `${days} days ago`;
}

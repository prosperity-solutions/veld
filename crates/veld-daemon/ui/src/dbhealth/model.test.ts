import { describe, expect, it } from "vitest";
import type { DbHealth } from "../api";
import { describeAge, noticeFor } from "./model";

const healthy: DbHealth = {
  checkedAt: "2026-08-26T08:00:00Z",
  database: { state: "ok", hits: 0, path: "/tmp/veld.db" },
  backups: {
    state: "ok",
    lastOk: "2026-08-26T07:56:00Z",
    consecutiveFailures: 0,
    intervalMinutes: 60,
  },
  restore: { restartsAutomatically: true },
};

const candidate = {
  path: "/tmp/veld-backups/veld-20260825T144826Z-v16.db",
  takenAt: "2026-08-25T14:48:26Z",
  schemaVersion: 16,
  bytes: 278_528,
  ownerOnly: true,
};

/** The payload the real incident would have produced. */
const damaged: DbHealth = {
  ...healthy,
  database: {
    state: "corrupt",
    detail: "database disk image is malformed",
    firstSeen: "2026-08-26T06:19:00Z",
    lastSeen: "2026-08-26T07:52:00Z",
    hits: 440,
    path: "/tmp/veld.db",
  },
  restore: { candidate, restartsAutomatically: true },
};

describe("noticeFor", () => {
  it("says nothing about a healthy database", () => {
    expect(noticeFor(healthy).severity).toBe("none");
    expect(noticeFor(healthy).marker).toBeNull();
  });

  /** Absence of data is not evidence of damage — an older daemon has no such
   *  endpoint, and a permanent banner there is a banner people learn to ignore. */
  it("says nothing when there is no health payload at all", () => {
    expect(noticeFor(null).severity).toBe("none");
    expect(noticeFor(undefined).severity).toBe("none");
  });

  it("reports a damaged database as an error, with a restore offered", () => {
    const notice = noticeFor(damaged);
    expect(notice.severity).toBe("error");
    expect(notice.headline).toBe("Veld's database is damaged");
    expect(notice.canRestore).toBe(true);
    expect(notice.marker).toBe("database damaged");
  });

  /** A button that cannot work is worse than no button: `backup::restore`
   *  refuses an artifact that fails its deep check, so the daemon sends no
   *  candidate and the banner must say so instead of offering the action. */
  it("does not offer a restore when no backup can be restored", () => {
    const notice = noticeFor({ ...damaged, restore: { restartsAutomatically: true } });
    expect(notice.severity).toBe("error");
    expect(notice.canRestore).toBe(false);
    expect(notice.detail).toContain("no usable backup");
  });

  /** A fault kind this bundle has never heard of still has to read as a fault.
   *  A cached bundle talking to a newer daemon is the normal case here, and
   *  narrowing an unknown state away is how a real fault renders as silence. */
  it("treats an unknown database state as a fault", () => {
    const notice = noticeFor({
      ...damaged,
      database: { ...damaged.database, state: "something-new" },
    });
    expect(notice.severity).toBe("error");
  });

  it("reports failing backups as a warning, and never offers a restore for them", () => {
    const notice = noticeFor({
      ...healthy,
      backups: {
        state: "failing",
        lastOk: "2026-08-25T14:48:26Z",
        lastError: "backup skipped — cannot open the database",
        consecutiveFailures: 17,
        intervalMinutes: 60,
      },
    });
    expect(notice.severity).toBe("warn");
    expect(notice.headline).toBe("Veld has stopped backing up");
    expect(notice.detail).toContain("cannot open the database");
    // Restoring would overwrite a *healthy* live database with an old copy.
    expect(notice.canRestore).toBe(false);
  });

  /** **The exact string the maintainer saw when driving this.** The daemon's
   *  derived deadman message restated the age and carried a raw RFC3339 stamp,
   *  and it was pasted after our own sentence:
   *
   *  "The last successful backup was 2 hours ago. no backup has been written
   *   since 2026-08-26T09:22:00+00:00 — one was due every 60 minute(s)"
   *
   *  Nothing had *attempted* a backup in that state, so there was no reason to
   *  append at all. */
  it("does not paste the derived overdue message after its own sentence", () => {
    const notice = noticeFor({
      ...healthy,
      backups: {
        state: "overdue",
        lastOk: "2026-08-26T09:22:00Z",
        // The deadman path records a message but no attempt.
        lastError:
          "no backup has been written since 2026-08-26T09:22:00+00:00 — one was due every 60 minute(s)",
        consecutiveFailures: 1,
        intervalMinutes: 60,
      },
    });
    expect(notice.detail).not.toContain("2026-08-26T09:22:00");
    expect(notice.detail).not.toContain("minute(s)");
    expect(notice.detail).not.toMatch(/\.\s+[a-z]/);
    expect(notice.detail).toContain("one is due every hour");
  });

  /** A real attempt that failed *is* worth quoting — that is the reason the
   *  reader needs — and it must not arrive uncapitalised mid-sentence. */
  it("quotes a failed attempt, capitalised", () => {
    const notice = noticeFor({
      ...healthy,
      backups: {
        state: "failing",
        lastOk: "2026-08-26T09:22:00Z",
        lastAttempt: "2026-08-26T11:22:00Z",
        lastError: "backup skipped — cannot open the database",
        consecutiveFailures: 3,
        intervalMinutes: 60,
      },
    });
    expect(notice.detail).toContain("The last attempt failed: Backup skipped");
    expect(notice.detail).not.toMatch(/\.\s+[a-z]/);
  });

  it("says the interval in words rather than as minute(s)", () => {
    const of = (intervalMinutes: number) =>
      noticeFor({
        ...healthy,
        backups: { state: "overdue", consecutiveFailures: 1, intervalMinutes },
      }).detail;
    expect(of(60)).toContain("every hour");
    expect(of(1440)).toContain("every 24 hours");
    expect(of(90)).toContain("every 90 minutes");
  });

  it("treats overdue backups the same way as failing ones", () => {
    const notice = noticeFor({
      ...healthy,
      backups: { ...healthy.backups, state: "overdue", consecutiveFailures: 1 },
    });
    expect(notice.severity).toBe("warn");
  });

  /** Backups switched off is a choice, not a fault, and `unknown` is just a
   *  daemon that has not reached its first tick. */
  it("stays quiet for backups that are off or not yet observed", () => {
    for (const state of ["off", "unknown"]) {
      const notice = noticeFor({
        ...healthy,
        backups: { ...healthy.backups, state, lastOk: null },
      });
      expect(notice.severity, state).toBe("none");
    }
  });

  /** Both are usually true at once — a database that cannot be read cannot be
   *  copied either — and the actionable one is the database. */
  it("prefers the database fault when both are broken", () => {
    const notice = noticeFor({
      ...damaged,
      backups: { ...healthy.backups, state: "failing", consecutiveFailures: 3 },
    });
    expect(notice.headline).toBe("Veld's database is damaged");
  });

  it("passes the pending notification through untouched", () => {
    const notify = { id: "corrupt", title: "t", body: "b" };
    expect(noticeFor({ ...damaged, notify }).notify).toEqual(notify);
    expect(noticeFor(damaged).notify).toBeNull();
  });

  /** A daemon that predates these fields sends a partial object; the renderer
   *  must not throw reading it. */
  it("survives a payload missing everything optional", () => {
    const sparse = {
      database: { state: "ok", hits: 0, path: "" },
      backups: { state: "ok", consecutiveFailures: 0, intervalMinutes: 60 },
      restore: { restartsAutomatically: false },
    } as DbHealth;
    expect(noticeFor(sparse).severity).toBe("none");
  });
});

describe("describeAge", () => {
  const now = new Date("2026-08-26T08:00:00Z");

  it("describes ages the way somebody deciding would read them", () => {
    expect(describeAge("2026-08-26T07:59:30Z", now)).toBe("just now");
    expect(describeAge("2026-08-26T07:30:00Z", now)).toBe("30 minutes ago");
    expect(describeAge("2026-08-26T07:00:00Z", now)).toBe("an hour ago");
    expect(describeAge("2026-08-26T03:00:00Z", now)).toBe("5 hours ago");
    expect(describeAge("2026-08-25T08:00:00Z", now)).toBe("yesterday");
    expect(describeAge("2026-08-20T08:00:00Z", now)).toBe("6 days ago");
  });

  /** The incident's own number: the newest backup was 17 hours old and nobody
   *  knew. It must not round to "yesterday" — that reads as fine. */
  it("keeps a same-day gap in hours", () => {
    expect(describeAge("2026-08-25T15:00:00Z", now)).toBe("17 hours ago");
  });

  it("does not pretend a broken or future stamp is an age", () => {
    expect(describeAge("not a date", now)).toBe("at an unknown time");
    expect(describeAge("2026-08-27T08:00:00Z", now)).toContain("future");
  });
});

import { describe, expect, it } from "vitest";

import type { ClientInfo } from "./channel";
import { awayNote, openableWorktrees, worktreeSetKey } from "./ownership";

const wt = (id: number, over: { trashed_at?: string } = {}) => ({
  id,
  trashed_at: "",
  ...over,
});

const none = () => false;

describe("which worktrees a client could take", () => {
  it("offers the plain ones", () => {
    expect(openableWorktrees([wt(1), wt(2)], none).map((w) => w.id)).toEqual([1, 2]);
  });

  /** Its panes would root a terminal and a browser at a directory that is
   *  leaving. The hunt used to offer these, because it read the repo's whole
   *  list. */
  it("never offers one in the trash", () => {
    const rows = [wt(1, { trashed_at: "2026-08-10T00:00:00Z" }), wt(2)];
    expect(openableWorktrees(rows, none).map((w) => w.id)).toEqual([2]);
  });

  /** The window that confirmed a removal knows before the daemon's flag does. */
  it("never offers one this window is removing", () => {
    const rows = [wt(1), wt(2)];
    expect(openableWorktrees(rows, (w) => w.id === 1).map((w) => w.id)).toEqual([2]);
  });

  it("drops the ones another client is showing, when asked to", () => {
    const rows = [wt(1), wt(2), wt(3)];
    const elsewhere = new Map<number, unknown>([[2, {}]]);
    expect(openableWorktrees(rows, none, elsewhere).map((w) => w.id)).toEqual([1, 3]);
    // …and keeps them when it is not: the daemon arbitrates, so a hunt asks
    // about a row whose claim may have been released a moment ago.
    expect(openableWorktrees(rows, none).map((w) => w.id)).toEqual([1, 2, 3]);
  });
});

describe("the set key an effect depends on", () => {
  /** The whole reason it exists: a poll hands back equal rows in new objects,
   *  and an effect keyed on the array re-runs on every one of them. */
  it("is the same for the same membership in new objects", () => {
    expect(worktreeSetKey([wt(1), wt(4)])).toBe(worktreeSetKey([wt(1), wt(4)]));
  });

  it("changes when a worktree joins or leaves", () => {
    const before = worktreeSetKey([wt(1)]);
    expect(worktreeSetKey([wt(1), wt(4)])).not.toBe(before);
    expect(worktreeSetKey([])).not.toBe(before);
  });

  /** Order is membership as far as this is concerned — the rail's order is the
   *  order candidates are tried in, so a reorder really is a different answer to
   *  "what would I take next". Stated so the looser reading is not assumed. */
  it("changes when the order does", () => {
    expect(worktreeSetKey([wt(4), wt(1)])).not.toBe(worktreeSetKey([wt(1), wt(4)]));
  });
});

describe("what a rail row says about a worktree that is elsewhere", () => {
  const info = (over: Partial<ClientInfo>): ClientInfo => ({
    kind: "browser",
    label: "",
    ...over,
  });

  it("names the holder", () => {
    expect(awayNote(info({ kind: "electron", label: "Veld Desktop" }))).toBe(
      "open in Veld Desktop",
    );
    expect(awayNote(info({ label: "Chrome" }))).toBe("open in Chrome");
  });

  /** A client too old to send a label still has to produce a true sentence, and
   *  the two kinds are not interchangeable — one can be raised by a click. */
  it("falls back to the kind when there is no label", () => {
    expect(awayNote(info({ kind: "electron" }))).toBe("open in another window");
    expect(awayNote(info({ kind: "browser" }))).toBe("open in another client");
    expect(awayNote(undefined)).toBe("open in another client");
  });
});

import { describe, expect, it } from "vitest";

import { acquireWorktree, type AcquireDeps, type AcquireTarget } from "./acquire";
import type { ClaimResult } from "./channel";

const wt = (id: number): AcquireTarget => ({ id, repo_root: "/repo" });

/**
 * A daemon that answers from a script, and a record of what happened.
 *
 * `cancelDuring` names the claim that is *in flight* when something newer
 * starts, which is how cancellation actually arrives — rather than a count of
 * `live()` calls, which pins the implementation's shape instead of its rule.
 * `cancelBefore` covers the case where it lands before anything is asked.
 */
function harness(
  answers: Record<number, ClaimResult>,
  opts: { candidates?: number[]; cancelDuring?: number; cancelBefore?: boolean } = {},
) {
  const asked: number[] = [];
  const released: number[] = [];
  const shown: number[] = [];
  let blocked = false;
  let cancelled = opts.cancelBefore === true;
  const deps: AcquireDeps = {
    claim: (id) => {
      asked.push(id);
      if (opts.cancelDuring === id) cancelled = true;
      return Promise.resolve(answers[id] ?? { ok: true });
    },
    release: (id) => released.push(id),
    candidates: () => (opts.candidates ?? []).map(wt),
    show: (t) => shown.push(t.id),
    blocked: () => {
      blocked = true;
    },
    live: () => !cancelled,
  };
  return {
    deps,
    asked,
    released,
    shown,
    get blocked() {
      return blocked;
    },
  };
}

const refused: ClaimResult = { ok: false, reason: "shown_elsewhere" };

describe("taking the preferred worktree", () => {
  it("shows it when it is granted", async () => {
    const h = harness({});
    await acquireWorktree(wt(7), h.deps);
    expect(h.asked).toEqual([7]);
    expect(h.shown).toEqual([7]);
    expect(h.released).toEqual([]);
  });

  /** Nothing is rendered before the daemon has answered — attaching to a
   *  worktree's PTY sessions takes them over. */
  it("never shows one that was refused", async () => {
    const h = harness({ 7: refused }, { candidates: [] });
    await acquireWorktree(wt(7), h.deps);
    expect(h.shown).toEqual([]);
  });
});

describe("what counts as a refusal worth hunting on", () => {
  it("hunts only on shown_elsewhere", async () => {
    const h = harness({ 7: refused }, { candidates: [7, 8, 9] });
    await acquireWorktree(wt(7), h.deps);
    expect(h.asked).toEqual([7, 8]);
    expect(h.shown).toEqual([8]);
  });

  /**
   * `superseded` means a later request from this client owns the outcome, and
   * `offline` means nothing was decided at all. Hunting on either moves the
   * window off a worktree for a transport failure — and the first candidate
   * would be "granted" the same way.
   */
  it.each(["superseded", "offline", "disconnected"])("stays put on %s", async (reason) => {
    const h = harness({ 7: { ok: false, reason } }, { candidates: [8, 9] });
    await acquireWorktree(wt(7), h.deps);
    expect(h.asked).toEqual([7]);
    expect(h.shown).toEqual([]);
    expect(h.blocked).toBe(false);
  });

  it("stops the hunt rather than skipping when a candidate answers something else", async () => {
    const h = harness(
      { 7: refused, 8: { ok: false, reason: "superseded" } },
      { candidates: [8, 9] },
    );
    await acquireWorktree(wt(7), h.deps);
    expect(h.asked).toEqual([7, 8]);
    expect(h.shown).toEqual([]);
    expect(h.blocked).toBe(false);
  });

  it("reports blocked only once every candidate is taken", async () => {
    const h = harness({ 7: refused, 8: refused, 9: refused }, { candidates: [8, 9] });
    await acquireWorktree(wt(7), h.deps);
    expect(h.asked).toEqual([7, 8, 9]);
    expect(h.blocked).toBe(true);
  });

  it("does not ask for the preferred worktree twice", async () => {
    const h = harness({ 7: refused, 8: refused }, { candidates: [7, 8] });
    await acquireWorktree(wt(7), h.deps);
    expect(h.asked).toEqual([7, 8]);
  });
});

describe("cancellation", () => {
  it("does nothing at all once something newer has started", async () => {
    const h = harness({}, { cancelBefore: true });
    await acquireWorktree(wt(7), h.deps);
    expect(h.asked).toEqual([]);
    expect(h.shown).toEqual([]);
  });

  /**
   * **The grant has to be given back.** Cancellation can arrive after the daemon
   * has recorded the claim, and a claim is otherwise undone only by taking
   * another or by disconnecting — so the worktree would sit greyed out in every
   * other client's rail, shown by a window that is showing nothing, and focus
   * that window when clicked. Two review angles found this hole independently.
   */
  it("gives back a worktree it was granted and then did not want", async () => {
    const h = harness({}, { cancelDuring: 7 });
    await acquireWorktree(wt(7), h.deps);
    expect(h.asked).toEqual([7]);
    expect(h.shown).toEqual([]);
    expect(h.released).toEqual([7]);
  });

  it("gives back a hunt's candidate too", async () => {
    const h = harness({ 7: refused }, { candidates: [8], cancelDuring: 8 });
    await acquireWorktree(wt(7), h.deps);
    expect(h.asked).toEqual([7, 8]);
    expect(h.shown).toEqual([]);
    expect(h.released).toEqual([8]);
  });

  it("does not report blocked to a caller that has moved on", async () => {
    const h = harness({ 7: refused, 8: refused }, { candidates: [8], cancelDuring: 8 });
    await acquireWorktree(wt(7), h.deps);
    expect(h.blocked).toBe(false);
  });
});

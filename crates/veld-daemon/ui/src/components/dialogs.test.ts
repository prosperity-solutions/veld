import { describe, expect, it } from "vitest";
import {
  branchForMode,
  createBlockers,
  sourceForMode,
  type SourceMode,
} from "./dialogs";
import type { RepoBranches } from "../api";

/**
 * The create dialog's mode-dependent derivations.
 *
 * They are tested here rather than through the component for the same reason
 * `StartConfig.test.ts` tests `startBody`/`resolveStartSelection`: the rules are
 * pure, they are the part that decides what git is asked to do, and two of them
 * have already been wrong once — the branch derivation (which must not slug an
 * existing ref) and the existing-branch guard (which fired only while the
 * branch field still held the picked remote's own name).
 */

const BRANCHES: RepoBranches = {
  local: [
    { name: "main", checked_out_in: "/repo", upstream: "origin/main" },
    { name: "feat/free", checked_out_in: null, upstream: null },
    { name: "feat/taken", checked_out_in: "/repo/../wt/taken", upstream: null },
  ],
  remote: [
    { name: "origin/main", local_name: "main", has_local: true },
    { name: "origin/feat/new", local_name: "feat/new", has_local: false },
  ],
};

const base = {
  derivedBranch: "checkout-v2",
  branchEdit: null as string | null,
  localBranch: "",
  remoteLocalName: null as string | null,
};

describe("branchForMode", () => {
  it("derives from the name for a new branch, until the user types", () => {
    expect(branchForMode({ ...base, mode: "new_branch" })).toBe("checkout-v2");
    expect(
      branchForMode({ ...base, mode: "new_branch", branchEdit: "feat/mine" }),
    ).toBe("feat/mine");
  });

  it("uses the picker verbatim for an existing local branch, never the slug", () => {
    // The whole reason this mode does not derive: `deriveBranch` would turn
    // `feature/JIRA-12` into a different ref, and checking out a ref that does
    // not exist fails with git's "invalid reference".
    expect(
      branchForMode({
        ...base,
        mode: "local_branch",
        localBranch: "feature/JIRA-12",
        branchEdit: "ignored",
      }),
    ).toBe("feature/JIRA-12");
  });

  it("defaults a remote checkout to the remote's short name and stays editable", () => {
    expect(
      branchForMode({
        ...base,
        mode: "remote_branch",
        remoteLocalName: "feat/new",
      }),
    ).toBe("feat/new");
    expect(
      branchForMode({
        ...base,
        mode: "remote_branch",
        remoteLocalName: "feat/new",
        branchEdit: "feat/renamed",
      }),
    ).toBe("feat/renamed");
    // Nothing picked yet: empty, not the name-derived guess, which would offer
    // to create a branch unrelated to any remote ref.
    expect(branchForMode({ ...base, mode: "remote_branch" })).toBe("");
  });

  it("treats an emptied field as untouched only via null, not via empty string", () => {
    // `""` is a real edit — the user cleared the box and has not blurred yet —
    // so Create must stay blocked rather than silently reverting to the guess.
    expect(
      branchForMode({ ...base, mode: "new_branch", branchEdit: "" }),
    ).toBe("");
  });

  it("derives from the name for a spin-off, like a plain new branch", () => {
    expect(branchForMode({ ...base, mode: "worktree" })).toBe("checkout-v2");
  });
});

describe("sourceForMode", () => {
  const args = { remoteRef: "origin/feat/new", fromWorktreeId: 42, carryOver: true };

  it("builds each variant with only its own fields", () => {
    expect(sourceForMode({ ...args, mode: "new_branch" })).toEqual({
      kind: "new_branch",
    });
    expect(sourceForMode({ ...args, mode: "local_branch" })).toEqual({
      kind: "local_branch",
    });
    expect(sourceForMode({ ...args, mode: "remote_branch" })).toEqual({
      kind: "remote_branch",
      remote_ref: "origin/feat/new",
    });
    expect(sourceForMode({ ...args, mode: "worktree" })).toEqual({
      kind: "worktree",
      from_worktree: 42,
      carry_over: true,
    });
  });

  it("sends carry_over explicitly, because the wire default is the opposite", () => {
    // The daemon's `#[serde(default)]` makes an omitted `carry_over` false,
    // while the dialog ticks it — so an unticked box has to travel as an
    // explicit `false` and not as an absent key.
    expect(sourceForMode({ ...args, mode: "worktree", carryOver: false })).toEqual({
      kind: "worktree",
      from_worktree: 42,
      carry_over: false,
    });
  });

  it("falls back to the daemon's 404 sentinel rather than targeting worktree 1", () => {
    expect(
      sourceForMode({ ...args, mode: "worktree", fromWorktreeId: null }),
    ).toEqual({ kind: "worktree", from_worktree: 0, carry_over: true });
  });
});

describe("createBlockers", () => {
  const args = {
    alias: "checkout-v2",
    branch: "feat/mine",
    aliasCollides: false,
    localBranch: "",
    remoteRef: "",
    fromWorktreeId: null as number | null,
    branches: BRANCHES,
  };

  it("lets the default create through", () => {
    expect(createBlockers({ ...args, mode: "new_branch" }).ready).toBe(true);
  });

  it("blocks a local branch that is already checked out, and names the holder", () => {
    const got = createBlockers({
      ...args,
      mode: "local_branch",
      localBranch: "feat/taken",
      branch: "feat/taken",
    });
    expect(got.localTaken).toBe("/repo/../wt/taken");
    expect(got.ready).toBe(false);
  });

  it("allows a free local branch", () => {
    const got = createBlockers({
      ...args,
      mode: "local_branch",
      localBranch: "feat/free",
      branch: "feat/free",
    });
    expect(got.localTaken).toBeNull();
    expect(got.ready).toBe(true);
  });

  it("blocks a NEW branch whose name a local branch already holds", () => {
    // The regression this guard was rewritten for: it must fire for whatever
    // is in the branch field, not only when that field still equals the picked
    // remote's short name.
    for (const mode of ["new_branch", "remote_branch", "worktree"] as SourceMode[]) {
      const got = createBlockers({
        ...args,
        mode,
        branch: "feat/free",
        remoteRef: "origin/feat/new",
        fromWorktreeId: 42,
      });
      expect(got.branchExistsLocally, mode).toBe(true);
      expect(got.ready, mode).toBe(false);
    }
  });

  it("does not treat the checked-out branch as a clash in local_branch mode", () => {
    // `local_branch` checks one out rather than creating it, so "it exists" is
    // the point, not a problem.
    expect(
      createBlockers({
        ...args,
        mode: "local_branch",
        localBranch: "feat/free",
        branch: "feat/free",
      }).branchExistsLocally,
    ).toBe(false);
  });

  it("stays silent about both clashes while the branch list is unknown", () => {
    // A failed or in-flight fetch must never block a create — least of all the
    // default one, which needs no list at all.
    const got = createBlockers({
      ...args,
      mode: "new_branch",
      branch: "feat/free",
      branches: null,
    });
    expect(got.branchExistsLocally).toBe(false);
    expect(got.localTaken).toBeNull();
    expect(got.ready).toBe(true);
  });

  it("requires each mode's own selection", () => {
    expect(createBlockers({ ...args, mode: "local_branch" }).ready).toBe(false);
    expect(
      createBlockers({ ...args, mode: "remote_branch", remoteRef: "" }).ready,
    ).toBe(false);
    expect(
      createBlockers({ ...args, mode: "worktree", fromWorktreeId: null }).ready,
    ).toBe(false);
    expect(
      createBlockers({ ...args, mode: "worktree", fromWorktreeId: 42 }).ready,
    ).toBe(true);
  });

  it("blocks an unusable name and an alias collision", () => {
    expect(createBlockers({ ...args, mode: "new_branch", alias: "" }).ready).toBe(
      false,
    );
    expect(
      createBlockers({ ...args, mode: "new_branch", aliasCollides: true }).ready,
    ).toBe(false);
    expect(createBlockers({ ...args, mode: "new_branch", branch: "" }).ready).toBe(
      false,
    );
  });
});

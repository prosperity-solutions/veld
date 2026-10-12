import { describe, expect, it } from "vitest";
import {
  baseBranchOptions,
  branchForMode,
  CURRENT_SOURCE,
  DEFAULT_BASE,
  defaultBase,
  chooseAgent,
  effectiveMode,
  effectiveName,
  createBlockers,
  sourceForMode,
  spinOffSource,
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
    { name: "main", refname: "refs/heads/main", checked_out_in: "/repo", upstream: "origin/main" },
    { name: "feat/free", refname: "refs/heads/feat/free", checked_out_in: null, upstream: null },
    { name: "feat/taken", refname: "refs/heads/feat/taken", checked_out_in: "/repo/../wt/taken", upstream: null },
  ],
  remote: [
    { name: "origin/main", refname: "refs/remotes/origin/main", local_name: "main", has_local: true },
    { name: "origin/feat/new", refname: "refs/remotes/origin/feat/new", local_name: "feat/new", has_local: false },
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
  const args = {
    startPoint: "",
    remoteRef: "origin/feat/new",
    fromPath: "/repo/../wt/source",
    carryOver: true,
  };

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
      from_path: "/repo/../wt/source",
      carry_over: true,
    });
  });

  it("sends a start point only when the base is not the default", () => {
    expect(
      sourceForMode({ ...args, mode: "new_branch", startPoint: "feat/base" }),
    ).toEqual({ kind: "new_branch", start_point: "feat/base" });
    // Only `new_branch` takes a base; the other modes name their own.
    expect(
      sourceForMode({ ...args, mode: "local_branch", startPoint: "feat/base" }),
    ).toEqual({ kind: "local_branch" });
  });

  it("sends carry_over explicitly, because the wire default is the opposite", () => {
    // The daemon's `#[serde(default)]` makes an omitted `carry_over` false,
    // while the dialog ticks it — so an unticked box has to travel as an
    // explicit `false` and not as an absent key.
    expect(sourceForMode({ ...args, mode: "worktree", carryOver: false })).toEqual({
      kind: "worktree",
      from_path: "/repo/../wt/source",
      carry_over: false,
    });
  });

  it("sends an empty path rather than resolving to some other checkout", () => {
    // The daemon 404s on `""`. `ready` blocks the submit before this can
    // happen, so this pins the behaviour of a request that escaped it anyway.
    expect(sourceForMode({ ...args, mode: "worktree", fromPath: null })).toEqual({
      kind: "worktree",
      from_path: "",
      carry_over: true,
    });
  });

  it("names the spin-off source by path, because the id is a reusable rowid", () => {
    // The #201 hazard: this dialog can sit open for minutes, and a rowid freed
    // by a permanent delete lands on the next worktree created.
    const got = sourceForMode({ ...args, mode: "worktree" });
    expect(JSON.stringify(got)).not.toMatch(/from_worktree/);
  });
});

describe("createBlockers", () => {
  const args = {
    alias: "checkout-v2",
    branch: "feat/mine",
    aliasCollides: false,
    localBranch: "",
    remoteRef: "",
    fromPath: null as string | null,
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
        fromPath: "/repo/../wt/source",
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
      createBlockers({ ...args, mode: "worktree", fromPath: null }).ready,
    ).toBe(false);
    expect(
      createBlockers({ ...args, mode: "worktree", fromPath: "/repo/../wt/s" })
        .ready,
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

describe("spinOffSource", () => {
  const wt = (id: number, path: string) => ({ id, path });

  it("resolves the picked source by path", () => {
    const sources = [wt(1, "/repo"), wt(2, "/wt/a")];
    expect(spinOffSource(sources, "/wt/a")).toEqual(wt(2, "/wt/a"));
  });

  it("resolves nothing for an empty selection", () => {
    expect(spinOffSource([wt(1, "/repo")], "")).toBeNull();
  });

  it("resolves nothing once the picked source is gone", () => {
    // The list is refreshed by the 5s poll while the dialog is open, so a
    // binned or deleted source has to read as *absent* — that is what disables
    // Create and shows the "no longer available" message.
    expect(spinOffSource([wt(1, "/repo")], "/wt/a")).toBeNull();
  });

  it("does NOT follow a reused rowid onto a different checkout", () => {
    // **The regression.** `worktrees.id` is a rowid with no AUTOINCREMENT: a
    // permanent delete frees the number and the next checkout created takes
    // it. Keyed on the id, this returned the impostor and its path went on the
    // wire — a branch cut from, and uncommitted work copied out of, a checkout
    // the user never picked.
    const picked = wt(7, "/wt/the-one-i-picked");
    const before = [wt(1, "/repo"), picked];
    expect(spinOffSource(before, picked.path)).toEqual(picked);

    // Same id, different checkout — what the next poll delivers.
    const impostor = wt(7, "/wt/somebody-elses");
    const after = [wt(1, "/repo"), impostor];
    expect(spinOffSource(after, picked.path)).toBeNull();
    expect(spinOffSource(after, picked.path)).not.toEqual(impostor);
  });
});

describe("effectiveName", () => {
  const free = () => true;

  it("returns a typed name untouched", () => {
    // Not trimmed, not slugged, not renumbered: the derivations downstream own
    // all three, and a collision in a name somebody chose is theirs to see.
    expect(effectiveName({ typed: "  Checkout V2 ", prompt: "", isFree: free })).toEqual({
      name: "  Checkout V2 ",
      auto: false,
    });
    // A typed name wins over a prompt that could have supplied one.
    expect(
      effectiveName({ typed: "Checkout V2", prompt: "Fix the redirect", isFree: free }),
    ).toEqual({ name: "Checkout V2", auto: false });
  });

  it("borrows from the prompt when the name is empty", () => {
    expect(
      effectiveName({
        typed: "",
        prompt: "Fix the login redirect loop so that sessions expire",
        isFree: free,
      }),
    ).toEqual({ name: "Fix the login redirect loop", auto: true });
    // Whitespace is not a name.
    expect(effectiveName({ typed: "   ", prompt: "Rewrite the parser", isFree: free })).toEqual(
      { name: "Rewrite the parser", auto: true },
    );
  });

  it("falls back when there is no prompt", () => {
    expect(effectiveName({ typed: "", prompt: "", isFree: free })).toEqual({
      name: "Workspace",
      auto: true,
    });
  });

  it("falls back when the prompt cannot supply an alias", () => {
    // The name would be perfectly readable and `deriveAlias` slugs it to `""`,
    // which would disable Create on a field the user was told to leave alone.
    expect(effectiveName({ typed: "", prompt: "Исправь редирект", isFree: free })).toEqual({
      name: "Workspace",
      auto: true,
    });
  });

  it("moves a generated name out of the way rather than colliding", () => {
    const taken = new Set(["Workspace", "Fix the redirect"]);
    expect(
      effectiveName({ typed: "", prompt: "", isFree: (n) => !taken.has(n) }),
    ).toEqual({ name: "Workspace 2", auto: true });
    expect(
      effectiveName({
        typed: "",
        prompt: "Fix the redirect",
        isFree: (n) => !taken.has(n),
      }),
    ).toEqual({ name: "Fix the redirect 2", auto: true });
  });

  it("never renumbers a typed name", () => {
    // The dialog reports that collision instead, next to the field it is about.
    expect(
      effectiveName({ typed: "Workspace", prompt: "", isFree: () => false }),
    ).toEqual({ name: "Workspace", auto: false });
  });
});

describe("chooseAgent", () => {
  const panes = [{ id: "claude" }, { id: "codex" }];

  it("defaults to the project's first pane before anything is picked", () => {
    // The order is `ide.panes`' own, which is the order the project wrote them
    // in — so the default is the project's stated preference, not a guess.
    expect(chooseAgent(panes, null)).toBe("claude");
  });

  it("keeps the user's pick", () => {
    expect(chooseAgent(panes, "codex")).toBe("codex");
  });

  it("falls back to the first pane when the pick has left the list", () => {
    // A `veld.json` edit or a lost `requires_bin` can remove a pane under an
    // open dialog. Returning the dead id would blank the Select and send a pane
    // name nothing will open.
    expect(chooseAgent(panes, "aider")).toBe("claude");
  });

  it("is null only when there is nothing to offer", () => {
    // Which is also the state in which the dialog renders no prompt at all, so
    // `launch` can never name a pane that does not exist.
    expect(chooseAgent([], null)).toBe(null);
    expect(chooseAgent([], "claude")).toBe(null);
  });
});

describe("effectiveMode", () => {
  it("is the chooser until something is chosen", () => {
    expect(effectiveMode({ chosen: null, hasAgents: true })).toBe("ask");
    expect(effectiveMode({ chosen: null, hasAgents: false })).toBe("ask");
  });

  it("renders the chosen mode when the project can honour it", () => {
    expect(effectiveMode({ chosen: "prompt", hasAgents: true })).toBe("prompt");
    expect(effectiveMode({ chosen: "manual", hasAgents: true })).toBe("manual");
  });

  it("falls through to manual when there is nothing to prompt", () => {
    // **The empty dialog.** The mode is a user preference and the agents are a
    // project fact, so they disagree for anyone who picked "Start with a prompt"
    // and then opened this in a repo with no agent panes — most repos. Rendering
    // `prompt` there gave a dialog with a mode switch, a Create button and an
    // empty body: the prompt column is gated on agents and the fields only
    // render under `manual`.
    expect(effectiveMode({ chosen: "prompt", hasAgents: false })).toBe("manual");
    // `manual` never needs an agent, so it is never redirected.
    expect(effectiveMode({ chosen: "manual", hasAgents: false })).toBe("manual");
  });
});

describe("baseBranchOptions", () => {
  const branches: RepoBranches = {
    local: [
      { name: "main", refname: "refs/heads/main", checked_out_in: "/repo", upstream: "origin/main" },
      { name: "feat/x", refname: "refs/heads/feat/x", checked_out_in: "/wt/x", upstream: null },
      { name: "heads/origin/odd", refname: "refs/heads/origin/odd", checked_out_in: null, upstream: null },
    ],
    remote: [
      { name: "origin/main", refname: "refs/remotes/origin/main", local_name: "main", has_local: true },
      { name: "remotes/origin/odd", refname: "refs/remotes/origin/odd", local_name: "odd", has_local: false },
      { name: "origin/feat/y", refname: "refs/remotes/origin/feat/y", local_name: "feat/y", has_local: false },
    ],
    default_branch: "main",
  };
  const args = {
    defaultLabel: "origin/main",
    defaultRef: "refs/remotes/origin/main" as string | null,
    branches,
    current: null,
  };
  const labels = (a: Parameters<typeof baseBranchOptions>[0]) =>
    baseBranchOptions(a).flatMap((g) => g.items.map((i) => i.label));

  it("puts the default first, then local, then remote branches", () => {
    const groups = baseBranchOptions(args);
    expect(groups.map((g) => g.group)).toEqual(["Default", "Local branches", "Remote branches"]);
    expect(groups[0].items).toEqual([{ value: DEFAULT_BASE, label: "origin/main" }]);
    // Sent as full refnames, shown short.
    expect(groups[1].items[0]).toEqual({ value: "refs/heads/main", label: "main" });
    expect(groups[2].items[0]).toEqual({ value: "refs/remotes/origin/odd", label: "remotes/origin/odd" });
  });

  it("never repeats a value, even for a local branch spelled like a remote one", () => {
    const values = baseBranchOptions(args).flatMap((g) => g.items.map((i) => i.value));
    expect(new Set(values).size).toBe(values.length);
    expect(values).toEqual(expect.arrayContaining(["refs/heads/origin/odd", "refs/remotes/origin/odd"]));
  });

  it("does not list the default's own ref a second time", () => {
    // origin/main is the default, fetched — not listed again as a stale remote.
    expect(labels(args).filter((l) => l === "origin/main")).toHaveLength(1);
    // In local mode the default is the main checkout's branch.
    expect(
      labels({ ...args, defaultLabel: "feat/x", defaultRef: "refs/heads/feat/x" }).filter((l) => l === "feat/x"),
    ).toHaveLength(1);
  });

  it("leaves a local branch alone that is merely spelled like the default", () => {
    const values = baseBranchOptions({
      ...args,
      branches: { ...branches, local: [...branches.local, { name: "heads/origin/main", refname: "refs/heads/origin/main", checked_out_in: null, upstream: null }] },
    }).flatMap((g) => g.items.map((i) => i.value));
    expect(values).toContain("refs/heads/origin/main");
    expect(values).not.toContain("refs/remotes/origin/main");
  });

  it("keeps origin/<default> as a remote when the default is something else", () => {
    expect(labels({ ...args, defaultLabel: "feat/parked", defaultRef: "refs/heads/feat/parked" })).toContain(
      "origin/main",
    );
  });

  it("still offers the default while the branch list is unknown", () => {
    expect(baseBranchOptions({ ...args, branches: null })).toEqual([
      { group: "Default", items: [{ value: DEFAULT_BASE, label: "origin/main" }] },
    ]);
  });

  it("names a non-branch source in a group of its own, above the default", () => {
    const groups = baseBranchOptions({ ...args, branches: null, current: "feat/x" });
    expect(groups[0]).toEqual({
      group: "Current",
      items: [{ value: CURRENT_SOURCE, label: "feat/x" }],
    });
    expect(groups[1].group).toBe("Default");
  });
});

describe("defaultBase", () => {
  const branches = (d: string | null, onOrigin = true): RepoBranches => ({
    local: [],
    remote: d && onOrigin ? [{ name: `origin/${d}`, refname: `refs/remotes/origin/${d}`, local_name: d, has_local: true }] : [],
    default_branch: d,
  });

  it("is origin/<default> in origin mode", () => {
    expect(defaultBase({ createFrom: "origin", branches: branches("main"), mainBranch: "feat/parked", failed: false })).toEqual({
      label: "origin/main",
      ref: "refs/remotes/origin/main",
      fallback: false,
      loading: false,
      failed: false,
      localOnly: false,
    });
  });

  it("names the main checkout's own branch in local mode, since that is where it lands", () => {
    expect(defaultBase({ createFrom: "local", branches: branches("main"), mainBranch: "feat/parked", failed: false }).label).toBe(
      "feat/parked",
    );
    expect(defaultBase({ createFrom: "local", branches: null, mainBranch: "", failed: false }).label).toBe("HEAD");
  });

  it("flags the HEAD fallback when origin mode finds no default branch", () => {
    expect(defaultBase({ createFrom: "origin", branches: branches(null), mainBranch: "develop", failed: false })).toEqual({
      label: "develop",
      ref: "refs/heads/develop",
      fallback: true,
      loading: false,
      failed: false,
      localOnly: false,
    });
  });

  it("does not claim origin for a default only this repo has", () => {
    expect(defaultBase({ createFrom: "origin", branches: branches("main", false), mainBranch: "feat", failed: false })).toEqual({
      label: "main",
      ref: "refs/heads/main",
      fallback: false,
      loading: false,
      failed: false,
      localOnly: true,
    });
  });

  it("does not guess a name before the list arrives, or once it has failed to", () => {
    expect(defaultBase({ createFrom: "origin", branches: null, mainBranch: "main", failed: false }).loading).toBe(true);
    const failed = defaultBase({ createFrom: "origin", branches: null, mainBranch: "main", failed: true });
    expect(failed.failed).toBe(true);
    expect(failed.loading).toBe(false);
  });
});

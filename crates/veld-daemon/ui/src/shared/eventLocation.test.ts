import { describe, expect, it } from "vitest";

import type { PaneLayout, PaneTab } from "../panes/model";
import { eventLocation } from "./eventLocation";

const A = "/src/alpha";
const B = "/src/beta";

const wt = (id: number, repo_root: string, alias: string, display_name?: string) => ({
  id,
  repo_root,
  alias,
  ...(display_name === undefined ? {} : { display_name }),
});

const WORKTREES = [wt(1, A, "main"), wt(2, B, "main"), wt(3, A, "wt3", "Spin-off")];
const REPOS = [
  { root: A, name: "alpha" },
  { root: B, name: "beta" },
];

/** A one-dock layout holding one terminal tab. */
const layoutWith = (tab: PaneTab): PaneLayout => ({
  docks: [
    { tabs: [tab], activeId: tab.id },
    { tabs: [], activeId: null },
  ],
  ratio: 0.5,
  focused: 0,
});

/** An ordinary terminal — no `spec`, so its name comes from its dock position. */
const terminal = (id: string, title = ""): PaneTab => ({ id, kind: "terminal", title });

/** A config-declared pane, whose `title` is the label the project gave it. */
const declared = (id: string, title: string): PaneTab => ({
  id,
  kind: "terminal",
  title,
  spec: "claude",
});

const locate = (over: Partial<Parameters<typeof eventLocation>[0]> = {}) =>
  eventLocation({
    worktreeId: 1,
    sessionId: "s1",
    worktrees: WORKTREES,
    repos: REPOS,
    activeRepoRoot: A,
    layouts: {},
    ...over,
  });

describe("naming the place an event happened", () => {
  /** On every line for the selected project, and saying nothing. */
  it("leaves the project out when it is the one on screen", () => {
    expect(locate({ worktreeId: 1, activeRepoRoot: A })).toBe("main");
  });

  /**
   * The case the project prefix exists for: markers and branch names repeat across
   * repos by design, so two projects both on `main` would otherwise be one name.
   */
  it("names the project when the event came from a different one", () => {
    expect(locate({ worktreeId: 2, activeRepoRoot: A })).toBe("beta · main");
  });

  it("prefers a worktree's display name over its alias", () => {
    expect(locate({ worktreeId: 3, activeRepoRoot: A })).toBe("Spin-off");
    expect(locate({ worktreeId: 3, activeRepoRoot: B })).toBe("alpha · Spin-off");
  });

  /** An agent hook reaches every client, including ones showing something else, so
   *  the worktree alone has to be enough on its own. */
  it("names no pane when this window has no layout for that worktree", () => {
    expect(locate({ worktreeId: 2, layouts: {} })).toBe("beta · main");
  });

  it("adds the pane when the layout is here", () => {
    expect(
      locate({ worktreeId: 1, sessionId: "s1", layouts: { 1: layoutWith(declared("s1", "Claude")) } }),
    ).toBe("main · Claude");
  });

  /** A session this window's layout does not contain — the pane was closed, or it
   *  belongs to a window that has it. The worktree still names the place. */
  it("names no pane for a session the layout does not hold", () => {
    expect(
      locate({ worktreeId: 1, sessionId: "gone", layouts: { 1: layoutWith(declared("s1", "Claude")) } }),
    ).toBe("main");
  });

  /**
   * `paneTabBaseLabel`, never `paneTabLabel` — #272. A shell's preexec hook writes
   * the running command into the terminal title, and a line naming that command is
   * read out of the context that would have explained it.
   */
  it("uses the pane's own name, not the title a process set for itself", () => {
    const noisy = terminal("s1", "sleep 5 && printf '\\033]9;done\\007'");
    // `Terminal`, unnumbered: `terminalLabel` numbers only once a dock holds more
    // than one. The point is the absence of the command line, not the digit.
    expect(locate({ worktreeId: 1, sessionId: "s1", layouts: { 1: layoutWith(noisy) } })).toBe(
      "main · Terminal",
    );
  });

  /**
   * Reachable from the notification path, which files whatever the daemon relays;
   * the Next unread button cannot reach it, since it picks its target from this
   * same list.
   */
  it("falls back to Veld for a worktree this client has never heard of", () => {
    expect(locate({ worktreeId: 99 })).toBe("Veld");
  });

  /** A repo row that has not loaded yet: name the worktree rather than prefixing
   *  it with an empty string and a separator. */
  it("drops the prefix when the project row is missing", () => {
    expect(locate({ worktreeId: 2, activeRepoRoot: A, repos: [] })).toBe("main");
  });
});

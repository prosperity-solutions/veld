import { describe, expect, it } from "vitest";

import {
  createInbox,
  isOsc9Notification,
  notifyKey,
  parseOsc133,
  supersedes,
  type Signal,
  type Source,
  type Unseen,
} from "./inbox";

/** The row's glyph, with the working indicator on unless a test says otherwise. */
const rowState = (
  box: ReturnType<typeof createInbox>,
  wt: number,
  showWorking = true,
) => box.rowState(wt, showWorking).state;

/** How many unread events a worktree has. The count is gone from the UI, not the store. */
const unread = (box: ReturnType<typeof createInbox>, wt: number) =>
  box.rowState(wt, false).entries.length;

const WT = 7;
const OTHER_WT = 9;
const NOW = 1_700_000_000_000;

/** A `C` then a `D;<exit>` — one whole command, the way a real shell emits it. */
const command = (exit: number): Signal[] => [
  { type: "osc133", mark: "C", exit: null },
  { type: "osc133", mark: "D", exit },
];

const agent = (
  state: "ready" | "working" | "blocked" | "idle" | "done",
  source: Source = "hook",
) =>
  ({ type: "agent", state, source }) as Signal;

describe("OSC 133 payloads", () => {
  it("reads a command end and its status", () => {
    expect(parseOsc133("D;0")).toEqual({ type: "osc133", mark: "D", exit: 0 });
    expect(parseOsc133("D;130")).toEqual({ type: "osc133", mark: "D", exit: 130 });
    expect(parseOsc133("C")).toEqual({ type: "osc133", mark: "C", exit: null });
    expect(parseOsc133("A")).toEqual({ type: "osc133", mark: "A", exit: null });
  });

  it("treats a status-less end as success rather than inventing a failure", () => {
    // A shell that omits the status is not reporting a failure. Defaulting to non-zero
    // would badge `failed` on every command in a half-configured terminal.
    expect(parseOsc133("D")).toEqual({ type: "osc133", mark: "D", exit: 0 });
    expect(parseOsc133("D;")).toEqual({ type: "osc133", mark: "D", exit: 0 });
    // Unparseable is unknown, which classifies as a failure with an unknown code —
    // something happened and it was not a clean success.
    expect(parseOsc133("D;banana")).toEqual({ type: "osc133", mark: "D", exit: null });
  });

  it("ignores marks it has no meaning for", () => {
    expect(parseOsc133("P;Cwd=/tmp")).toBeNull();
    expect(parseOsc133("")).toBeNull();
  });
});

describe("OSC 9 is not always a notification", () => {
  /**
   * The bug this prevents: `OSC 9;4` is the ConEmu progress sequence and Claude Code
   * emits it. Without this test the inbox raises `attention` on every progress tick of
   * the tool the feature exists to watch, and the toast beside it reads "4;1;50".
   */
  it("rejects the ConEmu progress sequence", () => {
    expect(isOsc9Notification("4;1;50")).toBe(false);
    expect(isOsc9Notification("4;0")).toBe(false);
    expect(isOsc9Notification("4")).toBe(false);
  });

  /**
   * The filter has to be narrow, not merely safe.
   *
   * "Starts with a digit" would be the lazy test and it would swallow "42 tests
   * passed" — a perfectly good notification. The rule is a *whole numeric field*
   * (`\d+` followed by `;` or end of string), which is what a progress sequence is and
   * what a sentence never is.
   */
  it("accepts a real message even when it starts with a number", () => {
    expect(isOsc9Notification("Build finished")).toBe(true);
    expect(isOsc9Notification("42 tests passed")).toBe(true);
    expect(isOsc9Notification("Done: 42 tests")).toBe(true);
    // Only a bare numeric field is refused.
    expect(isOsc9Notification("100")).toBe(false);
  });
});

describe("source authority", () => {
  it("never lets a passive signal override a hook", () => {
    expect(supersedes("hook", "detected")).toBe(true);
    expect(supersedes("hook", "socket")).toBe(true);
    expect(supersedes("socket", "detected")).toBe(true);
    expect(supersedes("detected", "hook")).toBe(false);
    expect(supersedes("socket", "hook")).toBe(false);
    // Equal authority lands: a second hook is newer news from the same mouth.
    expect(supersedes("hook", "hook")).toBe(true);
  });
});

describe("plain shell commands", () => {
  it("badges a command that succeeded and one that failed", () => {
    const box = createInbox();
    box.report("a", WT, command(0)[0], NOW);
    box.report("a", WT, command(0)[1], NOW);
    expect(box.unseen("a")?.kind).toBe("finished");

    box.report("b", WT, command(1)[0], NOW);
    box.report("b", WT, command(1)[1], NOW);
    expect(box.unseen("b")?.kind).toBe("failed");
    expect(box.unseen("b")?.detail).toContain("exit 1");
    expect(unread(box, WT)).toBe(2);
    expect(rowState(box, WT)).toBe("failed");
  });

  /**
   * The single most important test in this file, and the reason the classifier is a
   * state machine.
   *
   * Both shells emit a `D` the inbox must ignore, and bash cannot suppress it at all:
   * measured against bash 5.3, a bare Enter and the very first prompt each produce a
   * `D` carrying the *previous* command's status. If a `D` alone counted, an idle shell
   * would badge `failed` on every Enter — which is the "no false positives" acceptance
   * criterion, failed at the first keystroke.
   */
  it("ignores a command end that no command start preceded", () => {
    const box = createInbox();
    // bash's first prompt: `D;0` out of nowhere.
    box.report("a", WT, { type: "osc133", mark: "D", exit: 0 }, NOW);
    expect(box.unseen("a")).toBeNull();
    // A bare Enter, carrying a stale non-zero status from earlier.
    box.report("a", WT, { type: "osc133", mark: "D", exit: 1 }, NOW);
    expect(box.unseen("a")).toBeNull();
    expect(rowState(box, WT)).toBeNull();

    // A real command still lands, and the `C` is spent — a second `D` after it does not
    // produce a second event.
    for (const signal of command(0)) box.report("a", WT, signal, NOW);
    expect(box.unseen("a")?.kind).toBe("finished");
    box.read("a");
    box.report("a", WT, { type: "osc133", mark: "D", exit: 1 }, NOW);
    expect(box.unseen("a")).toBeNull();
  });

  /**
   * A long-running watcher must never badge, which is the other half of the
   * no-false-positives criterion.
   *
   * `pnpm dev` and `tsc --watch` emit a `C` when they start and no `D` until they stop.
   * That is the whole mechanism — there is nothing to suppress, and this test exists to
   * say so out loud, because a future "helpful" addition (a timer, an output-settling
   * heuristic) would break it silently.
   */
  it("stays silent for a command that has not ended", () => {
    const box = createInbox();
    box.report("dev", WT, { type: "osc133", mark: "C", exit: null }, NOW);
    expect(box.unseen("dev")).toBeNull();
    // Prompt marks in between change nothing.
    box.report("dev", WT, { type: "osc133", mark: "A", exit: null }, NOW);
    expect(unread(box, WT)).toBe(0);
    // …but it IS running, which is the working indicator's whole input.
    expect(box.isRunning("dev")).toBe(true);
    expect(rowState(box, WT)).toBe("working");
    // …and nothing at all with the indicator turned off.
    expect(rowState(box, WT, false)).toBeNull();
  });

  it("uses the process exit only where shell integration never spoke", () => {
    const box = createInbox();
    // No marks at all — an unsupported shell. The exit frame is the fallback.
    box.report("bare", WT, { type: "exit", code: 2 }, NOW);
    expect(box.unseen("bare")?.kind).toBe("failed");

    // A session that reported a command has already said what happened; the shell's own
    // exit is not a second event to badge.
    const box2 = createInbox();
    for (const signal of command(0)) box2.report("sh", WT, signal, NOW);
    expect(box2.unseen("sh")?.kind).toBe("finished");
    box2.report("sh", WT, { type: "exit", code: 1 }, NOW);
    expect(
      box2.unseen("sh")?.kind,
      "the exit frame must not overwrite the command's own result",
    ).toBe("finished");
  });
});

describe("coding agents", () => {
  it("badges attention when blocked and finished when the turn ends", () => {
    const box = createInbox();
    box.report("cc", WT, agent("blocked"), NOW);
    expect(box.unseen("cc")?.kind).toBe("attention");
    expect(rowState(box, WT)).toBe("attention");

    box.report("cc", WT, agent("idle"), NOW + 1);
    expect(box.unseen("cc")?.kind).toBe("finished");
    expect(rowState(box, WT)).toBe("finished");
  });

  /**
   * A `working` state retracts a stale attention instead of adding an event.
   *
   * The case: a permission prompt raises `attention`, and then it is answered somewhere
   * the inbox cannot see — in another window, by an auto-approval, by a timeout. The
   * badge is now a lie, and the user has no way to discover that except by opening the
   * pane, which is what the badge exists to save them.
   */
  it("retracts an attention the agent has moved on from", () => {
    const box = createInbox();
    box.report("cc", WT, agent("blocked"), NOW);
    expect(box.unseen("cc")?.kind).toBe("attention");
    box.report("cc", WT, agent("working"), NOW + 1);
    expect(box.unseen("cc")).toBeNull();

    // But a `finished` the user has not read yet is still true, so `working` leaves it.
    const box2 = createInbox();
    box2.report("cc", WT, agent("idle"), NOW);
    box2.report("cc", WT, agent("working"), NOW + 1);
    expect(box2.unseen("cc")?.kind).toBe("finished");
  });

  it("ignores a state it does not understand", () => {
    const box = createInbox();
    box.report("cc", WT, agent("blocked"), NOW);
    box.report("cc", WT, { type: "agent", state: "unknown", source: "hook" }, NOW + 1);
    expect(
      box.unseen("cc")?.kind,
      "an unrecognised state must not disturb a real one",
    ).toBe("attention");
  });

  /**
   * The authority rule, in the direction it actually breaks.
   *
   * An OSC 9 notification is "notice me" and nothing more. Arriving after a real `Stop`
   * hook it would flip a finished session back to "needs you" — and since Claude Code
   * emits OSC 9;4 progress reports, that would happen constantly.
   */
  it("does not let a notification override a hook, in either direction", () => {
    const box = createInbox();
    box.report("cc", WT, agent("idle"), NOW);
    box.report("cc", WT, { type: "notify", message: "hello" }, NOW + 1);
    expect(box.unseen("cc")?.kind).toBe("finished");

    // Nor does a `detected`-authority agent state displace a hook's.
    box.report("cc", WT, agent("blocked", "detected"), NOW + 2);
    expect(box.unseen("cc")?.kind).toBe("finished");
    // A hook does displace a hook.
    box.report("cc", WT, agent("blocked", "hook"), NOW + 3);
    expect(box.unseen("cc")?.kind).toBe("attention");
  });

  /** A command mark must not overwrite an agent's "needs you". */
  it("keeps an agent's attention when the shell reports a command ending", () => {
    const box = createInbox();
    box.report("cc", WT, agent("blocked"), NOW);
    for (const signal of command(0)) box.report("cc", WT, signal, NOW + 1);
    expect(box.unseen("cc")?.kind).toBe("attention");
  });

  it("badges a notification where no hook has ever spoken", () => {
    const box = createInbox();
    box.report("t", WT, { type: "notify", message: "  Build done  " }, NOW);
    const unseen = box.unseen("t");
    expect(unseen?.kind).toBe("attention");
    expect(unseen?.source).toBe("detected");
    expect(unseen?.detail).toBe("Build done");
  });
});

describe("reading", () => {
  it("clears one session on focus and leaves its neighbours alone", () => {
    const box = createInbox();
    for (const signal of command(0)) box.report("a", WT, signal, NOW);
    for (const signal of command(1)) box.report("b", WT, signal, NOW);
    expect(unread(box, WT)).toBe(2);

    expect(box.read("a")).toBe(true);
    expect(unread(box, WT)).toBe(1);
    // Reading a session with nothing unseen is not an error and reports so.
    expect(box.read("a")).toBe(false);
  });

  it("marks a whole worktree read without touching another", () => {
    const box = createInbox();
    for (const signal of command(0)) box.report("a", WT, signal, NOW);
    box.report("b", WT, agent("blocked"), NOW);
    for (const signal of command(1)) box.report("c", OTHER_WT, signal, NOW);

    box.markWorktreeRead(WT);
    expect(unread(box, WT)).toBe(0);
    expect(unread(box, OTHER_WT)).toBe(1);
  });

  /**
   * An event in the pane the user is looking at is not unseen.
   *
   * And the state machine still has to advance while watching, or switching away
   * mid-command loses the *next* event: the `C` arrives while watching, the `D` after
   * the user has gone.
   */
  it("does not badge the watched pane, and keeps its state machine running", () => {
    const box = createInbox();
    box.setWatching("a");
    for (const signal of command(1)) box.report("a", WT, signal, NOW);
    expect(box.unseen("a")).toBeNull();

    // A command starts while watched…
    box.report("a", WT, { type: "osc133", mark: "C", exit: null }, NOW + 1);
    // …the user switches away, and it ends. That event IS unseen.
    box.setWatching(null);
    box.report("a", WT, { type: "osc133", mark: "D", exit: 0 }, NOW + 2);
    expect(box.unseen("a")?.kind).toBe("finished");

    // Watching a pane that already has an unseen event reads it.
    box.setWatching("a");
    expect(box.unseen("a")).toBeNull();
  });

  it("notifies subscribers only when something changed", () => {
    const box = createInbox();
    let bumps = 0;
    const stop = box.subscribe(() => {
      bumps += 1;
    });
    // A `C` alone produces no *event* — but it does start the working state, and the
    // rail has a glyph for that, so it must re-render. (This assertion used to expect 0,
    // which was correct only while `working` did not exist.)
    box.report("a", WT, { type: "osc133", mark: "C", exit: null }, NOW);
    expect(bumps).toBe(1);
    box.report("a", WT, { type: "osc133", mark: "D", exit: 0 }, NOW);
    expect(bumps).toBe(2);
    box.read("a");
    expect(bumps).toBe(3);
    box.read("a");
    expect(bumps, "reading nothing must not re-render the rail").toBe(3);
    // A prompt mark changes neither, so it is silent — which is what stops every prompt
    // in every pane re-rendering the whole rail.
    box.report("a", WT, { type: "osc133", mark: "A", exit: null }, NOW);
    expect(bumps, "a prompt mark changes nothing and must be silent").toBe(3);
    stop();
    box.report("a", WT, agent("blocked"), NOW);
    expect(bumps, "an unsubscribed listener must not be called").toBe(3);
  });
});

describe("a shell that dies mid-command", () => {
  /**
   * The bug a bad test script found: `exit 3` exits the *shell*, so the pane went away
   * and the inbox reported **nothing at all**.
   *
   * The guard used to read `if (session.ranCommand || session.unseen) return undefined`,
   * on the theory that a session which had produced command marks had already said what
   * happened. But `ranCommand` does not mean "already reported" — it means *a command is
   * in flight right now*. A shell that dies mid-command will never send its `D`, so the
   * exit frame is the only report there is going to be, and suppressing it silenced the
   * single case that most needs a badge: a crash, an OOM kill, an `exit` inside a script.
   */
  it("reports the exit frame precisely because the D mark will never arrive", () => {
    const box = createInbox();
    // A command starts…
    box.report("a", WT, { type: "osc133", mark: "C", exit: null }, NOW);
    expect(box.isRunning("a")).toBe(true);
    // …and the whole shell dies instead of the command ending.
    box.report("a", WT, { type: "exit", code: 3 }, NOW + 1);
    expect(box.unseen("a")?.kind).toBe("failed");
    expect(box.unseen("a")?.detail).toContain("exit 3");
    // And it is no longer running — the process is gone whatever the last mark said.
    expect(box.isRunning("a")).toBe(false);
    expect(rowState(box, WT)).toBe("failed");
  });

  it("still does not double-report a command that already ended", () => {
    const box = createInbox();
    for (const signal of command(1)) box.report("sh", WT, signal, NOW);
    expect(box.unseen("sh")?.kind).toBe("failed");
    box.report("sh", WT, { type: "exit", code: 0 }, NOW + 1);
    expect(
      box.unseen("sh")?.detail,
      "an unread event already says what happened; the shell's own exit is not a second one",
    ).toContain("exit 1");
  });
});

describe("the row's one glyph", () => {
  /**
   * Precedence, in the order that matters.
   *
   * A rail row has room for one glyph, so a worktree with a blocked agent in pane 1 and
   * a finished build in pane 2 has to show the blocked one. `working` is last because it
   * is the only entry that is not news — a row where something is merely running must
   * not out-shout one where something is waiting.
   */
  it("shows the worst state a worktree has", () => {
    const box = createInbox();
    // Working alone.
    box.report("p1", WT, { type: "osc133", mark: "C", exit: null }, NOW);
    expect(rowState(box, WT)).toBe("working");
    // Finished beats working.
    for (const signal of command(0)) box.report("p2", WT, signal, NOW);
    expect(rowState(box, WT)).toBe("finished");
    // Failed beats finished.
    for (const signal of command(1)) box.report("p3", WT, signal, NOW);
    expect(rowState(box, WT)).toBe("failed");
    // Attention beats everything.
    box.report("p4", WT, agent("blocked"), NOW);
    expect(rowState(box, WT)).toBe("attention");

    // Reading the attention falls back to the next worst, rather than to nothing.
    box.read("p4");
    expect(rowState(box, WT)).toBe("failed");
  });

  /**
   * **Once an agent has spoken for a pane, the shell stops having an opinion about
   * whether it is working.**
   *
   * A pane running `claude` is one long shell command, so `ranCommand` stays true for
   * the entire session — which spun the working indicator the whole time the agent sat
   * idle waiting for a prompt. Technically correct and useless: what the user wants to
   * know is whether the *agent* is doing anything, and "a command is running" is a
   * lower-authority answer to a question the hook answers directly.
   *
   * Asserted in all three agent states against a `C` that never ends, because the bug
   * was invisible in any single one of them.
   */
  it("lets the agent, not the shell, decide whether a pane is working", () => {
    const box = createInbox();
    // The shell says a command is running, and nothing else has spoken yet.
    box.report("cc", WT, { type: "osc133", mark: "C", exit: null }, NOW);
    expect(box.isRunning("cc")).toBe(true);

    // A blocked agent is NOT working — it is stopped, waiting for a human.
    box.report("cc", WT, agent("blocked"), NOW + 1);
    expect(box.isRunning("cc")).toBe(false);
    expect(rowState(box, WT)).toBe("attention");

    // An idle agent is not working either, even though the shell still thinks its
    // `claude` command is in flight. This is the case that showed a permanent spinner.
    box.read("cc");
    box.report("cc", WT, agent("idle"), NOW + 2);
    box.read("cc");
    expect(box.isRunning("cc")).toBe(false);
    expect(
      rowState(box, WT),
      "an agent sitting at its prompt must leave the row quiet",
    ).toBeNull();

    // And a working agent is, so the indicator still has an input.
    box.report("cc", WT, agent("working"), NOW + 3);
    expect(box.isRunning("cc")).toBe(true);
    expect(rowState(box, WT)).toBe("working");
  });

  /** A pane with no agent keeps the shell's answer — the rule is authority, not silence. */
  it("still uses the shell for a pane no agent has spoken for", () => {
    const box = createInbox();
    box.report("build", WT, { type: "osc133", mark: "C", exit: null }, NOW);
    expect(box.isRunning("build")).toBe(true);
    expect(rowState(box, WT)).toBe("working");
  });

  it("hides the working state when the setting is off, and keeps real events", () => {
    const box = createInbox();
    box.report("p1", WT, { type: "osc133", mark: "C", exit: null }, NOW);
    expect(rowState(box, WT, true)).toBe("working");
    expect(rowState(box, WT, false)).toBeNull();

    for (const signal of command(1)) box.report("p2", WT, signal, NOW);
    expect(
      rowState(box, WT, false),
      "the setting hides activity, never an unread event",
    ).toBe("failed");
  });

  it("counts running panes, and stops counting one whose agent moved on", () => {
    const box = createInbox();
    box.report("a", WT, agent("working"), NOW);
    box.report("b", WT, { type: "osc133", mark: "C", exit: null }, NOW);
    expect(box.rowState(WT, true).running).toBe(2);
    box.report("a", WT, agent("idle"), NOW + 1);
    expect(box.rowState(WT, true).running).toBe(1);
  });

  /** Marking read must not stop anything — `working` is not an event to have seen. */
  it("keeps showing working after a worktree is marked read", () => {
    const box = createInbox();
    box.report("a", WT, { type: "osc133", mark: "C", exit: null }, NOW);
    for (const signal of command(0)) box.report("b", WT, signal, NOW);
    expect(rowState(box, WT)).toBe("finished");
    box.markWorktreeRead(WT);
    expect(box.hasUnread(WT)).toBe(false);
    expect(rowState(box, WT)).toBe("working");
  });
});

describe("which notification setting governs an event", () => {
  const unseen = (over: Partial<Unseen>): Unseen => ({
    kind: "finished",
    producer: "command",
    at: NOW,
    source: "detected",
    detail: "",
    ...over,
  });

  it("routes each of the four rows", () => {
    expect(notifyKey(unseen({ kind: "finished", producer: "command" }))).toBe(
      "activity.notifyCommandFinished",
    );
    expect(notifyKey(unseen({ kind: "failed", producer: "command" }))).toBe(
      "activity.notifyCommandFailed",
    );
    expect(notifyKey(unseen({ kind: "finished", producer: "agent" }))).toBe(
      "activity.notifyAgentFinished",
    );
    expect(notifyKey(unseen({ kind: "attention", producer: "agent" }))).toBe(
      "activity.notifyAgentWaiting",
    );
  });

  /**
   * An OSC 9 "notice me" from a plain program gets its OWN row, not the agent one.
   *
   * These were one row until the label had to say what it covered: "a coding agent is
   * waiting for you" firing for `curl` is a label that lies. The split is also what
   * keeps the OSC 9 banners that ship today firing — that row defaults on.
   */
  it("separates a program's notification from a blocked agent", () => {
    expect(notifyKey(unseen({ kind: "attention", producer: "command" }))).toBe(
      "activity.notifyNoticed",
    );
    expect(notifyKey(unseen({ kind: "attention", producer: "agent" }))).toBe(
      "activity.notifyAgentWaiting",
    );
  });

  /** A failed *agent* turn is not a thing veld can observe — see the module docs. */
  it("routes an agent's failure, should one ever exist, to its own row not a command's", () => {
    expect(notifyKey(unseen({ kind: "failed", producer: "agent" }))).toBe(
      "activity.notifyAgentFinished",
    );
  });
});

describe("one-shot event subscribers", () => {
  /**
   * A notification is an action, not a render. `subscribe` fires whenever anything
   * changed — including a read — and may fire more than once per event; `onEvent` fires
   * exactly once per new unseen event, which is what a banner needs.
   */
  it("fires once per new event, and never for a read or a retraction", () => {
    const box = createInbox();
    const seen: string[] = [];
    const stop = box.onEvent((e) => seen.push(`${e.sessionId}:${e.unseen.kind}`));

    for (const signal of command(0)) box.report("a", WT, signal, NOW);
    expect(seen).toEqual(["a:finished"]);

    // A read is not an event.
    box.read("a");
    expect(seen).toEqual(["a:finished"]);

    // Nor is a retraction.
    box.report("b", WT, agent("blocked"), NOW);
    box.report("b", WT, agent("working"), NOW + 1);
    expect(seen).toEqual(["a:finished", "b:attention"]);

    // Nor is a `C` mark, which changes only the working state.
    box.report("c", WT, { type: "osc133", mark: "C", exit: null }, NOW);
    expect(seen).toEqual(["a:finished", "b:attention"]);

    stop();
    for (const signal of command(1)) box.report("d", WT, signal, NOW);
    expect(seen, "an unsubscribed listener must not be called").toEqual([
      "a:finished",
      "b:attention",
    ]);
  });

  /** No banner for the pane the user is looking at. */
  it("does not fire for an event in the watched pane", () => {
    const box = createInbox();
    const seen: string[] = [];
    box.onEvent((e) => seen.push(e.sessionId));
    box.setWatching("a");
    for (const signal of command(0)) box.report("a", WT, signal, NOW);
    expect(seen).toEqual([]);
  });
});

describe("an agent that has just launched", () => {
  /**
   * `ready` claims the pane without reporting anything, and that is the whole point.
   *
   * The hole it closes: a pane running `claude` is one long shell command, so OSC 133 says
   * "a command is running here" for the entire session. Until some hook fires there is
   * nothing to contradict it, so a session sitting idle at its prompt showed the activity
   * spinner. No *hook* can fix that — none has run yet — so the wrapper reports it before
   * it execs.
   */
  it("silences the shell without putting anything in the inbox", () => {
    const box = createInbox();
    // The shell sees `claude` start, and speaks for the pane.
    box.report("cc", WT, { type: "osc133", mark: "C", exit: null }, NOW);
    expect(box.isRunning("cc")).toBe(true);
    expect(rowState(box, WT)).toBe("working");

    // The wrapper says an agent lives here and is idle.
    box.report("cc", WT, agent("ready"), NOW + 1);
    expect(box.isRunning("cc")).toBe(false);
    expect(
      rowState(box, WT),
      "a launched agent waiting for its first prompt must leave the row quiet",
    ).toBeNull();
    expect(
      box.unseen("cc"),
      "`ready` is not `idle` — mapping it to one would file a spurious 'agent finished' " +
        "on every single launch",
    ).toBeNull();
  });

  it("then follows the turn through to the end", () => {
    const box = createInbox();
    box.report("cc", WT, agent("ready"), NOW);
    // A prompt goes in.
    box.report("cc", WT, agent("working"), NOW + 1);
    expect(rowState(box, WT)).toBe("working");
    // It needs an answer.
    box.report("cc", WT, agent("blocked"), NOW + 2);
    expect(rowState(box, WT)).toBe("attention");
    expect(box.isRunning("cc")).toBe(false);
    // It finishes.
    box.report("cc", WT, agent("idle"), NOW + 3);
    expect(rowState(box, WT)).toBe("finished");
  });
});

describe("surviving a page reload", () => {
  /**
   * The events come back; the live state does not, and must not.
   *
   * `ranCommand` is a claim about a process *right now*, and a reload is exactly when it
   * stops being knowable — the command may have finished while the page was gone.
   * Restoring it would show an activity spinner with nothing left that could ever clear
   * it, because the marks that would have are in the scrollback and its replay is
   * deliberately suppressed.
   */
  it("carries unread events over but never the running state", () => {
    const before = createInbox();
    for (const signal of command(1)) before.report("a", WT, signal, NOW);
    before.report("b", WT, agent("blocked"), NOW + 1);
    // A pane mid-command when the page went away.
    before.report("c", WT, { type: "osc133", mark: "C", exit: null }, NOW + 2);
    expect(before.isRunning("c")).toBe(true);

    const after = createInbox();
    after.restore(JSON.parse(JSON.stringify(before.snapshot())));
    expect(after.unseen("a")?.kind).toBe("failed");
    expect(after.unseen("b")?.kind).toBe("attention");
    expect(rowState(after, WT)).toBe("attention");
    expect(
      after.isRunning("c"),
      "a restored spinner would have nothing left that could ever stop it",
    ).toBe(false);
  });

  it("merges rather than replaces, so a live event outranks a restored one", () => {
    const box = createInbox();
    const stored = (() => {
      const other = createInbox();
      other.report("a", WT, agent("idle"), NOW);
      return JSON.parse(JSON.stringify(other.snapshot()));
    })();
    // A pane that already reported something newer while the restore was in flight.
    box.report("a", WT, agent("blocked"), NOW + 5);
    box.restore(stored);
    expect(box.unseen("a")?.kind).toBe("attention");
  });

  it("drops a row it cannot typecheck without losing the rest", () => {
    const box = createInbox();
    box.restore({
      v: 1,
      sessions: {
        good: {
          worktreeId: WT,
          unseen: {
            kind: "failed",
            producer: "command",
            source: "detected",
            at: NOW,
            detail: "Command failed (exit 1)",
          },
        },
        // Every one of these is a shape an older build, a newer build or a hand edit
        // could produce. One bad row must cost that row and nothing else.
        badKind: { worktreeId: WT, unseen: { kind: "exploded", producer: "command", source: "detected", at: NOW, detail: "x" } },
        badProducer: { worktreeId: WT, unseen: { kind: "failed", producer: "ghost", source: "detected", at: NOW, detail: "x" } },
        badSource: { worktreeId: WT, unseen: { kind: "failed", producer: "command", source: "vibes", at: NOW, detail: "x" } },
        noWorktree: { unseen: { kind: "failed", producer: "command", source: "detected", at: NOW, detail: "x" } },
        notAnObject: 42,
        nullEntry: null,
      },
    });
    expect(box.known()).toEqual(["good"]);
    expect(box.unseen("good")?.detail).toContain("exit 1");
  });

  it("survives a document that is not one at all", () => {
    const box = createInbox();
    for (const doc of [null, undefined, 42, "nope", [], {}, { sessions: 7 }]) {
      box.restore(doc);
    }
    expect(box.known()).toEqual([]);
  });

  /** A restored event for a pane that is gone could never be read by looking. */
  it("retains only the panes the layouts still name", () => {
    const box = createInbox();
    box.report("kept", WT, agent("blocked"), NOW);
    box.report("closed", WT, agent("blocked"), NOW);
    box.retain(["kept"], [WT]);
    expect(box.known()).toEqual(["kept"]);
    expect(rowState(box, WT)).toBe("attention");
    box.retain([], [WT]);
    expect(rowState(box, WT)).toBeNull();
  });

  /**
   * **The boot sequence, which is what actually broke.**
   *
   * Reported from real use: the restore worked and the rail was still empty. The cause was
   * the prune guarding it. `readLayouts` is explicit that *a main window gets nothing from
   * storage* — its layouts arrive from the daemon, asynchronously, one worktree at a time —
   * so the effect that prunes runs its first pass with `layouts === {}`. A prune that
   * dropped everything not in that empty set deleted the entire restored inbox before a
   * single pane had mounted.
   *
   * So the test walks the real order: restore, prune with nothing known, *then* the
   * layouts arrive. A unit test that called `retain` with the final layouts could never
   * have caught it.
   */
  it("survives a prune that runs before any layout has arrived", () => {
    const stored = (() => {
      const before = createInbox();
      for (const signal of command(0)) before.report("a", WT, signal, NOW);
      before.report("b", OTHER_WT, agent("blocked"), NOW);
      return JSON.parse(JSON.stringify(before.snapshot()));
    })();

    const box = createInbox();
    box.restore(stored);
    // First pass: no layouts yet. `Object.keys({})` is empty, and so is the id list.
    box.retain([], []);
    expect(
      box.known().sort(),
      "a window that knows no layouts knows nothing about which panes exist",
    ).toEqual(["a", "b"]);
    expect(rowState(box, WT)).toBe("finished");
    expect(rowState(box, OTHER_WT)).toBe("attention");

    // One worktree's layout arrives, still naming its pane. The other is untouched
    // because nothing has been learned about it yet.
    box.retain(["a"], [WT]);
    expect(box.known().sort()).toEqual(["a", "b"]);

    // …and now it arrives without that pane, so the stale entry finally goes.
    box.retain([], [WT]);
    expect(box.known()).toEqual(["b"]);
    expect(rowState(box, WT)).toBeNull();
    expect(rowState(box, OTHER_WT)).toBe("attention");
  });

  it("snapshots only what is unread", () => {
    const box = createInbox();
    for (const signal of command(0)) box.report("read", WT, signal, NOW);
    box.report("unread", WT, agent("blocked"), NOW);
    box.read("read");
    // A pane that is merely running carries no event, so it is not in the snapshot.
    box.report("busy", WT, { type: "osc133", mark: "C", exit: null }, NOW);
    expect(Object.keys(box.snapshot().sessions)).toEqual(["unread"]);
  });
});

describe("defects found in review", () => {
  /**
   * An agent must hand the pane back when it goes.
   *
   * `agentSource` was set by the first hook and never cleared, so the authority claim
   * outlived the agent for the life of the pane. Two things stayed broken afterwards, and
   * the second is not gated by any setting: the shell's `working` was permanently muted,
   * and **every later OSC 9 in that pane was dropped**, because the notify arm refuses to
   * speak where an agent has. Reusing a pane after an agent session is the ordinary case.
   */
  it("gives the pane back to the shell when the agent session ends", () => {
    const box = createInbox();
    box.report("p", WT, agent("ready"), NOW);
    box.report("p", WT, agent("done"), NOW + 1);
    expect(box.unseen("p")?.detail).toBe("Agent session ended");
    box.read("p");

    // The shell speaks for this pane again.
    box.report("p", WT, { type: "osc133", mark: "C", exit: null }, NOW + 2);
    expect(box.isRunning("p")).toBe(true);
    expect(rowState(box, WT)).toBe("working");
    box.report("p", WT, { type: "osc133", mark: "D", exit: 1 }, NOW + 3);
    expect(box.unseen("p")?.kind).toBe("failed");
    box.read("p");

    // …and so does an OSC 9, which is the half no setting could have restored.
    box.report("p", WT, { type: "notify", message: "tests failed" }, NOW + 4);
    expect(box.unseen("p")?.detail).toBe("tests failed");
  });

  /** A pane's process exit clears the claim too — the agent died with the shell. */
  it("gives the pane back when the whole process exits", () => {
    const box = createInbox();
    box.report("p", WT, agent("blocked"), NOW);
    box.report("p", WT, { type: "exit", code: 0 }, NOW + 1);
    box.read("p");
    box.report("p", WT, { type: "notify", message: "later" }, NOW + 2);
    expect(box.unseen("p")?.detail).toBe("later");
  });

  /**
   * A command mark must not speak over a hook's event, whatever kind it is.
   *
   * The guard covered only `attention`, so: `claude` finishes a turn (`Stop` → "Agent
   * finished" + a banner), the user Ctrl-Cs out of it, zsh emits `D;130` with `ranCommand`
   * still true from the launch `C` — and that replaced the agent's event with "Command
   * failed (exit 130)" plus a **second** banner, claiming a failure that never happened.
   */
  it("does not let a command mark overwrite a hook's finished event", () => {
    const box = createInbox();
    box.report("cc", WT, agent("ready"), NOW);
    box.report("cc", WT, { type: "osc133", mark: "C", exit: null }, NOW + 1);
    box.report("cc", WT, agent("idle"), NOW + 2);
    expect(box.unseen("cc")?.detail).toBe("Agent finished");

    let events = 0;
    box.onEvent(() => {
      events += 1;
    });
    // Ctrl-C: the shell's own mark for the `claude` command it has been running.
    box.report("cc", WT, { type: "osc133", mark: "D", exit: 130 }, NOW + 3);
    expect(
      box.unseen("cc")?.detail,
      "the agent's own account of what happened must survive",
    ).toBe("Agent finished");
    expect(events, "and it must not fire a second notification").toBe(0);
  });

  /**
   * **The `done` path, which the first version of the fix above missed.**
   *
   * That fix widened the "a hook owns this pane" guard, and a separate fix released the
   * claim when the agent reported `done` — which disarmed the guard for the `D` mark the
   * shell emits moments later, when it redraws its prompt after `claude` exits. So "Agent
   * session ended" was immediately overwritten by "Command finished", or by "Command failed
   * (exit 130)" plus a second banner if the user had Ctrl-C'd out.
   *
   * The test that was supposed to cover this used `idle`, which never released the claim —
   * so it passed while the `done` path stayed broken. Hence this one, and hence the claim
   * now being handed back by the `D` that closes the agent's own launch.
   */
  it("keeps an ended agent's own account when the shell's prompt mark follows", () => {
    const box = createInbox();
    box.report("cc", WT, agent("ready"), NOW);
    box.report("cc", WT, { type: "osc133", mark: "C", exit: null }, NOW + 1);
    box.report("cc", WT, agent("done"), NOW + 2);
    expect(box.unseen("cc")?.detail).toBe("Agent session ended");
    // A departed agent is not "working", so the row must not spin while the mark is pending.
    expect(box.isRunning("cc")).toBe(false);

    let events = 0;
    box.onEvent(() => {
      events += 1;
    });
    // The shell's mark for the `claude` command that just ended — Ctrl-C's variant, which
    // is the one that claimed a failure that never happened.
    box.report("cc", WT, { type: "osc133", mark: "D", exit: 130 }, NOW + 3);
    expect(box.unseen("cc")?.detail).toBe("Agent session ended");
    expect(events, "and no second banner").toBe(0);

    // …and that same mark is when the pane becomes the shell's again, so an OSC 9 after it
    // is heard. No read required: the hand-off is a fact about which command ended.
    box.report("cc", WT, { type: "notify", message: "back to normal" }, NOW + 4);
    expect(box.unseen("cc")?.detail).toBe("back to normal");
  });

  /**
   * **The hand-off must not depend on when the user reads.**
   *
   * Two earlier versions tied it to a read — one released the claim on `done` (which
   * disarmed the guard for the `D` that follows a moment later), the other on the read
   * itself, which only narrowed that to a race whose width is the agent's shutdown time.
   * Marking the worktree read in that gap put the spurious "Command failed (exit 130)"
   * straight back. This walks that exact interleaving.
   */
  it("survives a read landing between the agent's exit and the shell's prompt", () => {
    const box = createInbox();
    box.report("cc", WT, agent("ready"), NOW);
    box.report("cc", WT, { type: "osc133", mark: "C", exit: null }, NOW + 1);
    box.report("cc", WT, agent("done"), NOW + 2);
    // The user reads it from the rail before the shell has redrawn its prompt.
    box.markWorktreeRead(WT);
    expect(box.hasUnread(WT)).toBe(false);
    // Now the mark arrives.
    box.report("cc", WT, { type: "osc133", mark: "D", exit: 130 }, NOW + 3);
    expect(
      box.unseen("cc"),
      "the agent's command closing is not a new event, whenever the read happened",
    ).toBeNull();
  });

  /**
   * The claim must not get stuck when the `done` event is discarded as already-seen.
   *
   * `report` drops an event for the pane the user is watching, and the deferred-release
   * version had no hook there — so ending an agent session *while looking at its pane*, the
   * ordinary way to do it, left the pane claimed forever: no activity ever again, and every
   * later OSC 9 in it silently dropped.
   */
  it("hands the pane back even when the agent's last event was never filed", () => {
    const box = createInbox();
    box.setWatching("cc");
    box.report("cc", WT, agent("ready"), NOW);
    box.report("cc", WT, { type: "osc133", mark: "C", exit: null }, NOW + 1);
    box.report("cc", WT, agent("done"), NOW + 2);
    expect(box.unseen("cc")).toBeNull();
    // The shell's mark for the launch closes the hand-off.
    box.report("cc", WT, { type: "osc133", mark: "D", exit: 0 }, NOW + 3);
    box.setWatching(null);
    box.report("cc", WT, { type: "notify", message: "heard" }, NOW + 4);
    expect(box.unseen("cc")?.detail).toBe("heard");
  });

  /** A relaunch in the same pane re-claims it, and a read must not hand it back. */
  it("keeps a live agent's claim across a read", () => {
    const box = createInbox();
    box.report("cc", WT, agent("ready"), NOW);
    box.report("cc", WT, { type: "osc133", mark: "C", exit: null }, NOW + 1);
    box.report("cc", WT, agent("done"), NOW + 2);
    box.report("cc", WT, { type: "osc133", mark: "D", exit: 0 }, NOW + 3);
    box.read("cc");

    // Relaunched in the same pane.
    box.report("cc", WT, agent("ready"), NOW + 4);
    box.report("cc", WT, agent("blocked"), NOW + 5);
    expect(box.unseen("cc")?.producer).toBe("agent");
    box.read("cc");
    // The agent is still live, so its own OSC 9 must not be taken as a command's.
    box.report("cc", WT, { type: "notify", message: "needs permission" }, NOW + 6);
    expect(
      box.unseen("cc"),
      "a read must not hand a live agent's pane back to the shell",
    ).toBeNull();
  });

  /**
   * **The claim can be observed before the mark that preceded it.**
   *
   * The inbox sees signals in the order xterm delivers them, not the order the bytes
   * arrived: an agent's state arrives on the IDE channel and is filed synchronously, while a
   * mark arrives on the pty socket and is parsed by xterm's write buffer, which defers to a
   * macrotask. So the wrapper's launch report can be seen *before* the `preexec` mark that
   * actually came first — and attributing the launch on only one of the two orders filed a
   * spurious "Command failed" for the agent's own exit and wedged the claim permanently.
   */
  it("attributes an agent's launch whichever order the two signals are seen in", () => {
    for (const claimFirst of [false, true]) {
      const box = createInbox();
      const claim = () => box.report("cc", WT, agent("ready"), NOW);
      const mark = () =>
        box.report("cc", WT, { type: "osc133", mark: "C", exit: null }, NOW);
      if (claimFirst) {
        claim();
        mark();
      } else {
        mark();
        claim();
      }
      // Ctrl-C out of the agent: its own launch closing, not a command that failed.
      box.report("cc", WT, { type: "osc133", mark: "D", exit: 130 }, NOW + 1);
      expect(
        box.unseen("cc"),
        `claimFirst=${claimFirst}: the agent's own exit is not a command failure`,
      ).toBeNull();
      // …and the pane is the shell's again, which is the half that used to wedge.
      box.report("cc", WT, { type: "notify", message: "heard" }, NOW + 2);
      expect(
        box.unseen("cc")?.detail,
        `claimFirst=${claimFirst}: the claim must not outlive the agent`,
      ).toBe("heard");
    }
  });

  /**
   * A late fire-and-forget hook must not claim the *next* command's mark.
   *
   * `Stop` and `SessionEnd` do not block, so one can land after the shell has already
   * started the command the user typed ahead. Latching on "any agent signal while any `C` is
   * outstanding" stole that command's mark — swallowing its result, its spinner and every
   * OSC 9 in the pane until it ended.
   */
  it("does not let a late agent hook claim the next command", () => {
    const box = createInbox();
    // An agent runs and exits; its launch mark is correctly swallowed.
    box.report("cc", WT, { type: "osc133", mark: "C", exit: null }, NOW);
    box.report("cc", WT, agent("ready"), NOW + 1);
    box.report("cc", WT, agent("working"), NOW + 2);
    box.report("cc", WT, { type: "osc133", mark: "D", exit: 130 }, NOW + 3);

    // The typed-ahead command starts…
    box.report("cc", WT, { type: "osc133", mark: "C", exit: null }, NOW + 4);
    // …and the agent's `SessionEnd` only now arrives.
    box.report("cc", WT, agent("done"), NOW + 5);
    box.read("cc");

    expect(box.isRunning("cc"), "the new command is still running").toBe(true);
    box.report("cc", WT, { type: "notify", message: "3 failing" }, NOW + 6);
    expect(box.unseen("cc")?.detail, "its notifications must be heard").toBe("3 failing");
    box.read("cc");
    box.report("cc", WT, { type: "osc133", mark: "D", exit: 1 }, NOW + 7);
    expect(box.unseen("cc")?.kind, "and its own result must be filed").toBe("failed");
  });

  /** Restarting a pane drops the previous run's unread verdict. */
  it("does not leave a restarted pane asserting the old run's failure", () => {
    const box = createInbox();
    box.report("p", WT, { type: "exit", code: 1 }, NOW);
    expect(box.unseen("p")?.kind).toBe("failed");
    // Restart from the tab strip, without reading — reachable for an inactive tab.
    box.restarted("p");
    expect(
      box.unseen("p"),
      "a badge from the previous run asserts a failure this run has not had",
    ).toBeNull();
    box.report("p", WT, { type: "exit", code: 0 }, NOW + 1);
    expect(box.unseen("p")?.kind).toBe("finished");
  });

  /**
   * A restart reuses the pane id, so the exit marker must not survive it.
   *
   * `restartTerminal`/`startTerminal` delete the daemon session and connect a new shell
   * under the *same* id. With the marker left in place, every exit after the first was
   * dropped silently — no badge, no notification, not even a re-render — which is the
   * primary event path for a `oneshot` pane, the pane kind the exit producer exists for.
   */
  it("reports the exit of a pane that was restarted under the same id", () => {
    const box = createInbox();
    box.report("p", WT, { type: "exit", code: 1 }, NOW);
    expect(box.unseen("p")?.kind).toBe("failed");
    box.read("p");

    // Restart: same id, new process.
    box.restarted("p");
    box.report("p", WT, { type: "exit", code: 1 }, NOW + 1);
    expect(
      box.unseen("p")?.kind,
      "the second run's failure is news, not a replay of the first",
    ).toBe("failed");

    // …and the replay-suppression still works within that run.
    box.read("p");
    box.report("p", WT, { type: "exit", code: 1 }, NOW + 2);
    expect(box.unseen("p")).toBeNull();
  });

  /**
   * An agent state this build does not understand is a no-op, not a claim.
   *
   * It used to pass the authority check and set `agentSource` before falling out of the
   * switch — so a newer daemon sending `compacting` permanently muted the shell's
   * `working` and every OSC 9 in that pane, which is the opposite of the wire contract's
   * "an unrecognised state is dropped by the store".
   */
  it("claims nothing for a state it does not understand", () => {
    const box = createInbox();
    box.report("p", WT, { type: "osc133", mark: "C", exit: null }, NOW);
    box.report(
      "p",
      WT,
      { type: "agent", state: "compacting" as never, source: "hook" },
      NOW + 1,
    );
    expect(box.isRunning("p"), "the shell must still speak for this pane").toBe(true);
    box.report("p", WT, { type: "notify", message: "still heard" }, NOW + 2);
    expect(box.unseen("p")?.detail).toBe("still heard");
  });

  /**
   * The daemon replays the `exit` frame to **every** attach — pinned on the Rust side by
   * `reattaching_after_exit_reports_the_exit`. So a read event came back on every page
   * reload, with a notification, for a command that died before it.
   */
  it("does not re-file an exit it has already reported", () => {
    const box = createInbox();
    box.report("p", WT, { type: "exit", code: 1 }, NOW);
    expect(box.unseen("p")?.kind).toBe("failed");
    box.read("p");

    // The same frame again, as a fresh attach receives it.
    box.report("p", WT, { type: "exit", code: 1 }, NOW + 1);
    expect(box.unseen("p")).toBeNull();

    // And across a reload, which is the case that actually bit: the marker rides the
    // snapshot even though there is no event left to restore.
    const after = createInbox();
    after.restore(JSON.parse(JSON.stringify(box.snapshot())));
    after.report("p", WT, { type: "exit", code: 1 }, NOW + 2);
    expect(
      after.unseen("p"),
      "a reload must not resurrect an exit the user already read",
    ).toBeNull();
  });

  /** An OSC 9 payload is arbitrary terminal output, and it is now persisted. */
  it("bounds a program's own notification text", () => {
    const box = createInbox();
    box.report("p", WT, { type: "notify", message: "x".repeat(50_000) }, NOW);
    const detail = box.unseen("p")?.detail ?? "";
    expect(detail.length).toBeLessThanOrEqual(201);
    expect(detail.endsWith("…")).toBe(true);
    // A short message is untouched.
    box.read("p");
    box.report("p", WT, { type: "notify", message: "  done  " }, NOW + 1);
    expect(box.unseen("p")?.detail).toBe("done");
  });
});

/**
 * The same answer for a set of worktrees, which is what a project is.
 *
 * The project selector shows one glyph per project, and it has to mean what the rail's
 * means or the user learns two languages for one vocabulary.
 */
describe("a whole project's glyph", () => {
  const THIRD_WT = 11;

  it("shows the worst state across every worktree in the set", () => {
    const box = createInbox();
    const project = new Set([WT, OTHER_WT]);
    box.report("p1", WT, { type: "osc133", mark: "C", exit: null }, NOW);
    expect(box.groupState(project, true).state).toBe("working");
    // A finished build in the *other* worktree still beats working in this one.
    for (const signal of command(0)) box.report("p2", OTHER_WT, signal, NOW);
    expect(box.groupState(project, true).state).toBe("finished");
    // …and a blocked agent anywhere in the set beats everything.
    box.report("p3", WT, agent("blocked"), NOW);
    expect(box.groupState(project, true).state).toBe("attention");
  });

  /** The point of taking a set: a worktree outside it contributes nothing, which is
   *  what keeps one project's news off another project's row. */
  it("ignores worktrees outside the set", () => {
    const box = createInbox();
    box.report("p1", THIRD_WT, agent("blocked"), NOW);
    expect(box.groupState(new Set([WT, OTHER_WT]), true).state).toBe(null);
    expect(box.groupState(new Set([THIRD_WT]), true).state).toBe("attention");
  });

  /**
   * Newest-first **across** the set, not per worktree.
   *
   * This is why merging N `rowState` results in the caller is not the same thing: each
   * of those is sorted on its own, and concatenating already-sorted runs interleaves
   * them wrong — here that would put the older event of worktree A ahead of the newer
   * event of worktree B.
   */
  it("orders the whole set's events newest first", () => {
    const box = createInbox();
    box.report("a-old", WT, agent("idle"), NOW);
    box.report("b-new", OTHER_WT, agent("blocked"), NOW + 2000);
    box.report("a-mid", THIRD_WT, agent("done"), NOW + 1000);
    expect(
      box
        .groupState(new Set([WT, OTHER_WT, THIRD_WT]), false)
        .entries.map((e) => e.sessionId),
    ).toEqual(["b-new", "a-mid", "a-old"]);
  });

  it("counts running panes across the set", () => {
    const box = createInbox();
    box.report("p1", WT, { type: "osc133", mark: "C", exit: null }, NOW);
    box.report("p2", OTHER_WT, { type: "osc133", mark: "C", exit: null }, NOW);
    expect(box.groupState(new Set([WT, OTHER_WT]), true).running).toBe(2);
    // `showWorking` gates the glyph, never the count — same rule as a rail row.
    expect(box.groupState(new Set([WT, OTHER_WT]), false).state).toBe(null);
  });

  /** A project with no countable worktrees — every one of them trashed, or a repo
   *  whose rows have not loaded — has nothing to say rather than everything. */
  it("says nothing for an empty set", () => {
    const box = createInbox();
    box.report("p1", WT, agent("blocked"), NOW);
    expect(box.groupState(new Set(), true).state).toBe(null);
    expect(box.groupState(new Set(), true).entries).toEqual([]);
  });
});

describe("bookkeeping", () => {
  it("orders a worktree's events newest first", () => {
    const box = createInbox();
    box.report("old", WT, agent("idle"), NOW);
    box.report("new", WT, agent("blocked"), NOW + 1000);
    expect(box.rowState(WT, false).entries.map((e) => e.sessionId)).toEqual(["new", "old"]);
  });

  it("follows a session that moves worktree, rather than counting it twice", () => {
    // Not a normal flow — the daemon refuses a cross-worktree attach — but the store is
    // keyed on the session, and a stale worktree id would leave a permanent phantom
    // badge on a worktree with nothing in it.
    const box = createInbox();
    box.report("a", WT, agent("blocked"), NOW);
    box.report("a", OTHER_WT, agent("blocked"), NOW + 1);
    expect(unread(box, WT)).toBe(0);
    expect(rowState(box, OTHER_WT)).toBe("attention");
  });

  it("forgets a closed session", () => {
    const box = createInbox();
    box.report("a", WT, agent("blocked"), NOW);
    box.forget("a");
    expect(rowState(box, WT)).toBeNull();
    expect(box.known()).toEqual([]);
  });

  it("counts a worktree it has never heard of as empty", () => {
    const box = createInbox();
    expect(box.rowState(1234, true)).toEqual({ state: null, entries: [], running: 0 });
    expect(box.unseen("nope")).toBeNull();
  });
});

describe("hasAgent", () => {
  // What the terminal's paste handling reads to decide whether an image should be
  // handed over as an image (`^V`, which only an agent understands) or written down
  // and named by path — see `panes/terminalPaste.ts`'s `imageAction`. The property
  // that makes it usable for that is liveness, asserted below.
  it("is false for a session nothing has reported about", () => {
    const box = createInbox();
    box.register("cc", WT);
    expect(box.hasAgent("cc")).toBe(false);
    // And for one it has never heard of at all.
    expect(box.hasAgent("nope")).toBe(false);
  });

  it("becomes true as soon as an agent reports", () => {
    const box = createInbox();
    box.register("cc", WT);
    box.report("cc", WT, agent("working"), NOW);
    expect(box.hasAgent("cc")).toBe(true);
  });

  it("goes false again when the agent's command ends", () => {
    // **The load-bearing half.** The wrapper `exec`s the real binary, so it can
    // never report its own exit; the shell's `D` mark is what retires the claim.
    // Without this, a pane that once ran an agent would look like one forever, and
    // a `^V` at the shell prompt that followed would corrupt the next character
    // the user types in zsh.
    const box = createInbox();
    box.register("cc", WT);
    // The real launch sequence: the shell's `C` for `claude`, then the wrapper's
    // own `ready`, which is what *claims* that command for the agent. Reporting
    // `working` here instead would not claim it — only `ready` may, because only
    // `ready` means "an agent just started" rather than "an agent is busy".
    box.report("cc", WT, { type: "osc133", mark: "C", exit: null }, NOW);
    box.report("cc", WT, agent("ready"), NOW + 1);
    box.report("cc", WT, agent("working"), NOW + 2);
    expect(box.hasAgent("cc")).toBe(true);
    box.report("cc", WT, { type: "osc133", mark: "D", exit: 0 }, NOW + 3);
    expect(box.hasAgent("cc"), "the agent's claim must not outlive its command").toBe(false);
  });

  it("is per session, so one pane's agent does not speak for another", () => {
    const box = createInbox();
    box.register("cc", WT);
    box.register("shell", WT);
    box.report("cc", WT, agent("working"), NOW);
    expect(box.hasAgent("cc")).toBe(true);
    expect(box.hasAgent("shell")).toBe(false);
  });

  it("is false again after a restart, which replaces the pane's process", () => {
    const box = createInbox();
    box.register("cc", WT);
    box.report("cc", WT, agent("working"), NOW);
    box.restarted("cc");
    expect(box.hasAgent("cc")).toBe(false);
  });
});

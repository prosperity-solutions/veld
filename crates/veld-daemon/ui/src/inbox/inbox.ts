/**
 * The WorktreeInbox: what happened while you weren't looking, and does any of it
 * need you.
 *
 * Not a status indicator. A status indicator answers "what is this pane doing right
 * now", which you can see by looking at it. This answers a question you cannot: of the
 * panes you are *not* looking at, which finished, which failed, and which is waiting
 * for you. So its unit is an **unseen event**, it is read by looking, and marking a
 * worktree read is an explicit gesture — the shape of an inbox, not of a light.
 *
 * # Two producers, one seam
 *
 * Everything arrives through {@link WorktreeInbox.report}. Today:
 *
 * - **Plain shell commands**, via OSC 133 semantic prompt marks that veld's shell
 *   integration injects (`veld-daemon/src/pty/shims.rs`). Exact command start and end,
 *   with the exit code. Parsed by xterm, which is already parsing the bytes.
 * - **Coding agents**, via lifecycle hooks the agent itself runs, relayed by the daemon
 *   over the IDE channel. An agent's state is an application-level fact that its output
 *   does not contain — see `veld_core::agent` for the measurement.
 *
 * # An unseen event and a live state are different things
 *
 * `finished`/`failed`/`attention` are **events**: they happened, they are unread, and
 * looking at the pane reads them. `working` is a **state**: it is true while a command
 * or an agent is running and it is never "read", it simply stops being true. They live
 * in separate fields for that reason, and {@link WorktreeInbox.rowState} is what
 * collapses both into the one glyph a rail row has room for.
 *
 * # Why this lives in the browser, and what that costs
 *
 * The OSC 133 parser is xterm, so command events exist only while a window is showing
 * the pane. Moving the store into the daemon would not change that — it would need a
 * VT scanner in the PTY holder, which is the named follow-up — so a daemon-side store
 * would buy durability for the agent half only, at the price of a persistence question
 * and a round trip per command. The honest shape is that **both producers are as
 * durable as the window**: a closed tab loses them, and so does a command that *spans* a
 * reload — its start mark is gone with the page, and the scrollback replay that would carry
 * it is deliberately suppressed, so the end mark arrives unpaired and is discarded. Unread
 * events themselves do survive a reload (see `persist.ts`).
 *
 * # Source authority
 *
 * A state is only as good as what told us. `hook > socket > detected`, and a
 * lower-authority signal never displaces a higher one — otherwise a stray OSC 9
 * notification arriving after a real `Stop` hook would flip a finished session back to
 * "needs you", and the badge would stop meaning anything.
 *
 * # What is deliberately absent: quiescence
 *
 * No output-settling heuristic. It cannot distinguish a working agent from a waiting
 * one, and it was measured false-positiving on blinking cursors and silent builds. A
 * signal that says "maybe something needs you" on `sleep 30` teaches people to ignore
 * the badge, which costs more than the events it catches.
 */

/** What kind of unseen event this is. */
export type UnseenKind = "finished" | "failed" | "attention";

/**
 * What produced an event.
 *
 * Not the same question as {@link Source}, which is *how reliably* we know. This is
 * *what happened*, and it exists because the notification table needs it: "a command
 * finished" and "a coding agent finished" are different enough that one switch for both
 * was the wrong shape.
 */
export type Producer = "command" | "agent";

/**
 * How a signal was learned, in strictly increasing authority.
 *
 * `socket` has no producer yet. It exists because a tool that reports continuously over
 * a connection of its own is a different kind of claim from one that fires a hook, and
 * collapsing the two would mean re-deciding this ordering when the first one arrives.
 */
export type Source = "detected" | "socket" | "hook";

const AUTHORITY: Record<Source, number> = { detected: 0, socket: 1, hook: 2 };

/** Whether a state learned from `next` may replace one already known from `current`. */
export function supersedes(next: Source, current: Source): boolean {
  // Equal authority supersedes: a second hook is newer news from the same mouth.
  return AUTHORITY[next] >= AUTHORITY[current];
}

/** What a coding agent reported. Mirrors `veld_core::agent::State`. */
export type AgentState =
  /** The wrapper's report, before any hook: an agent is here and it is idle. */
  | "ready"
  | "working"
  | "blocked"
  | "idle"
  | "done"
  | "unknown";

/** One thing that happened in a terminal session. */
export type Signal =
  /** An OSC 133 semantic prompt mark. `exit` is set only for `D`. */
  | { type: "osc133"; mark: "A" | "B" | "C" | "D"; exit: number | null }
  /** The pane's process ended. */
  | { type: "exit"; code: number | null }
  /** An OSC 9 / 777 / kitty 99 notification. Lowest authority — see {@link Source}. */
  | { type: "notify"; message: string }
  /** A coding agent said what it is doing. */
  | { type: "agent"; state: AgentState; source: Source };

/** An unseen event, as the rail renders it. */
export interface Unseen {
  kind: UnseenKind;
  producer: Producer;
  /** `Date.now()` when it landed, for ordering and for the tooltip. */
  at: number;
  source: Source;
  /** One short line naming what happened, for the tooltip. */
  detail: string;
}

/**
 * What a rail row shows: one glyph, worst-state-wins.
 *
 * `working` is last because it is the only entry that is not news — a row whose only
 * signal is "something is running" must not out-shout one with a blocked agent in it.
 */
export type RowState = "attention" | "failed" | "finished" | "working";

/** Highest first. A row renders the first of these it has. */
const PRECEDENCE: RowState[] = ["attention", "failed", "finished", "working"];

/** What a whole worktree currently has to say, and the detail lines behind it. */
export interface RowSummary {
  state: RowState | null;
  /** Unread events, newest first, with the pane each belongs to. */
  entries: { sessionId: string; unseen: Unseen }[];
  /** How many of this worktree's panes are running something. */
  running: number;
}

const NOTHING: RowSummary = { state: null, entries: [], running: 0 };

/** Agent states this build acts on. Anything else claims nothing — see `classify`. */
const KNOWN_AGENT_STATES: AgentState[] = ["ready", "working", "blocked", "idle", "done"];

/**
 * How much of a program's own message to keep.
 *
 * Generous for a banner, bounded for storage: the text reaches a system notification and a
 * `sessionStorage` write, and neither wants an unbounded string from terminal output.
 */
const MAX_DETAIL = 200;

function bannerText(message: string): string {
  const trimmed = message.trim();
  return trimmed.length > MAX_DETAIL ? `${trimmed.slice(0, MAX_DETAIL)}…` : trimmed;
}

/**
 * What the classifier remembers per session.
 *
 * `ranCommand` is the C-before-D rule, and it is the reason this is a state machine
 * rather than a pure function per signal. Both shells emit a `D` mark the inbox must
 * ignore: bash emits one for a bare Enter and for its very first prompt, carrying a
 * *stale* exit status, because `PROMPT_COMMAND` has no idea whether a command ran.
 * Measured, not predicted. So a `D` only counts when a `C` came first.
 */
interface SessionState {
  worktreeId: number;
  /** A `C` mark has been seen and its `D` has not arrived yet. */
  ranCommand: boolean;
  /** The agent in this pane said it is working. */
  agentWorking: boolean;
  /** The authority of the last agent state accepted for this session. */
  agentSource: Source | null;
  /**
   * The outstanding `C` mark belongs to an agent's own launch.
   *
   * This is the whole of the agent/shell hand-off, and it took three attempts to find. A
   * pane running `claude` is one shell command: the `C` is the launch and the `D` that
   * eventually closes it is the *same event* the agent already reported through a hook. So
   * that one `D` must be dropped, and the pane handed back to the shell at the same moment.
   *
   * Keyed on the command rather than on when the user reads, which is what the two previous
   * versions got wrong. Releasing the claim on `done` disarmed the guard for the `D` that
   * follows a moment later; releasing it on the *read* only moved the race — marking the
   * worktree read in the gap between the agent exiting and the shell redrawing its prompt
   * put the spurious "Command failed (exit 130)" back. A fact about which command is
   * outstanding has no such window.
   */
  agentCommand: boolean;
  /**
   * Whether this pane's process exit has already been filed.
   *
   * The daemon replays the `exit` frame to **every** attach, so without this a read event
   * came back on every page reload — with a notification. Persisted, because the reload is
   * exactly when it has to be remembered.
   */
  reportedExit: boolean;
  unseen: Unseen | null;
}

/**
 * Whether a session is running something, and **the agent has the last word**.
 *
 * A pane running `claude` is one long shell command, so the shell's view of it is
 * "a command is in flight" for the entire session — which would spin the working
 * indicator the whole time an agent sat idle waiting for a prompt. Technically true
 * and useless: what the user wants to know is whether the *agent* is doing anything.
 *
 * So once a hook has spoken for a session (`agentSource !== null`), the shell's
 * `ranCommand` stops contributing. This is the same authority rule the rest of the
 * module runs on — a low-authority signal never overrides a high-authority one — applied
 * to the live state rather than to the unseen event.
 */
function running(session: SessionState): boolean {
  if (session.agentSource !== null) return session.agentWorking;
  return session.ranCommand;
}

/** A new unseen event, for a consumer that wants to act on it once (a notification). */
export interface InboxEvent {
  sessionId: string;
  worktreeId: number;
  unseen: Unseen;
}

/**
 * The inbox.
 *
 * A plain module-level store with a listener set, read through functions rather than a
 * snapshot object — the pattern `panes/terminalHost.ts` and `panes/browserHost.ts`
 * already use in this app, and consumed the same way (`useReducer` bump + a read call).
 * Deliberately **not** `useSyncExternalStore`: that would need `getSnapshot` to return a
 * referentially stable value or React throws error #185, which means caching a snapshot
 * and invalidating it on every mutation — a whole class of bug this app has no other
 * instance of. There is nothing `useSyncExternalStore` would buy here that the
 * established pattern does not.
 */
class WorktreeInbox {
  private sessions = new Map<string, SessionState>();
  private listeners = new Set<() => void>();
  /**
   * Subscribers that want each new event *once*, rather than the fact that something
   * changed. Separate from {@link listeners} because a notification is an action, not a
   * render: a render may run any number of times for one event, and a banner may not.
   */
  private eventListeners = new Set<(e: InboxEvent) => void>();
  /**
   * The session the user is actually looking at, or null.
   *
   * An event in the watched pane is not unseen — the user is watching it happen. Set by
   * the app, because "is the user looking at this" needs the focused pane *and* the
   * focused window, and only the app knows both.
   */
  private watching: string | null = null;

  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  /** Called once per new unseen event. Returns an unsubscribe. */
  onEvent(fn: (e: InboxEvent) => void): () => void {
    this.eventListeners.add(fn);
    return () => this.eventListeners.delete(fn);
  }

  private notify(): void {
    for (const fn of this.listeners) fn();
  }

  /** Which pane the user is looking at. Reads it, if it had anything unseen. */
  setWatching(sessionId: string | null): void {
    this.watching = sessionId;
    if (sessionId !== null) this.read(sessionId);
  }

  /** A session's worktree, so an event can be filed before any signal arrives. */
  register(sessionId: string, worktreeId: number): void {
    const existing = this.sessions.get(sessionId);
    if (existing) {
      existing.worktreeId = worktreeId;
      return;
    }
    this.sessions.set(sessionId, {
      worktreeId,
      ranCommand: false,
      agentWorking: false,
      agentSource: null,
      agentCommand: false,
      reportedExit: false,
      unseen: null,
    });
  }

  /**
   * File a signal.
   *
   * `now` is injected so the tests are not time-dependent.
   */
  report(
    sessionId: string,
    worktreeId: number,
    signal: Signal,
    now: number = Date.now(),
  ): void {
    this.register(sessionId, worktreeId);
    const session = this.sessions.get(sessionId);
    if (!session) return;
    session.worktreeId = worktreeId;

    const before = session.unseen;
    const wasRunning = running(session);
    const event = this.classify(session, signal, now);
    const nowRunning = running(session);

    if (event !== undefined) {
      // The watched pane's events are seen as they happen. Still classified above,
      // because the state machine has to track the `C` even when its `D` will be
      // discarded — otherwise switching to a pane mid-command loses the next event.
      session.unseen = sessionId === this.watching ? null : event;
    }
    if (session.unseen !== before) {
      this.notify();
      // Only a genuinely new event, and only one that stuck. A read, a retraction, or
      // an event in the pane the user is watching must not raise a banner.
      if (session.unseen !== null && session.unseen === event) {
        const payload: InboxEvent = { sessionId, worktreeId, unseen: session.unseen };
        for (const fn of this.eventListeners) fn(payload);
      }
    } else if (wasRunning !== nowRunning) {
      // "Working" changed without any unseen event changing — a command started, or a
      // command the user is watching ended. The rail still has to re-render.
      this.notify();
    }
  }

  /**
   * A signal's effect on this session: an event, `null` to clear, or `undefined` for
   * "nothing happened".
   *
   * The three-way answer is load-bearing. `undefined` and `null` are different: a `C`
   * mark changes the state machine and produces no event, while a `working` agent state
   * actively *retracts* an attention that is no longer true.
   */
  private classify(
    session: SessionState,
    signal: Signal,
    now: number,
  ): Unseen | null | undefined {
    switch (signal.type) {
      case "osc133":
        if (signal.mark === "C") {
          session.ranCommand = true;
          return undefined;
        }
        if (signal.mark !== "D") return undefined; // `A`/`B` are prompt boundaries.
        // The C-before-D rule. An idle shell, and bash's very first prompt, both emit a
        // `D` — with whatever status was last set — and neither is a command that ran.
        if (!session.ranCommand) return undefined;
        session.ranCommand = false;
        // This `D` closes the command an agent was running, so it is not a second event —
        // it is the shell's account of something the agent already reported through a
        // hook, and the agent's is the better one. Dropping it is also the honest moment
        // to hand the pane back: the agent's command has ended and the shell is at a
        // prompt again, so from here the shell speaks for this pane.
        //
        // Ctrl-C out of a finished `claude` is the case that made this visible: it emits
        // `D;130`, which used to replace "Agent finished" with "Command failed (exit 130)"
        // and fire a second banner claiming a failure that never happened.
        if (session.agentCommand) {
          session.agentCommand = false;
          session.agentSource = null;
          session.agentWorking = false;
          return undefined;
        }
        // And separately: an unread "needs you" from a hook is never buried by a command
        // ending. Narrow on purpose — only `attention`, and only from a hook — because it
        // is a claim about *relative value* rather than about the same event twice: an
        // agent waiting on you is the highest-value thing this feature detects, and a
        // command's verdict can wait behind it. Widening this to every kind is what made
        // the release-ordering bug above hard to see.
        if (session.agentSource === "hook" && session.unseen?.kind === "attention") {
          return undefined;
        }
        return signal.exit === 0
          ? {
              kind: "finished",
              producer: "command",
              at: now,
              source: "detected",
              detail: "Command finished",
            }
          : {
              kind: "failed",
              producer: "command",
              at: now,
              source: "detected",
              detail: `Command failed (exit ${signal.exit ?? "?"})`,
            };

      case "exit": {
        // The pane's process is gone, so nothing is running in it any more — whatever
        // the shell last told us.
        const midCommand = session.ranCommand;
        session.ranCommand = false;
        session.agentWorking = false;
        // And no agent owns this pane any more. Leaving `agentSource` set outlived the
        // agent for the life of the pane and muted two things permanently: the shell's
        // `working` (see `running`) and **every OSC 9 in that pane**, because the `notify`
        // arm below refuses to speak where an agent has. Reusing a pane after an agent
        // session is the ordinary case, not an exotic one.
        session.agentSource = null;
        session.agentCommand = false;
        // The daemon **replays the exit frame to every new attach** — pinned by
        // `reattaching_after_exit_reports_the_exit`, which asserts a second attach reads
        // the same code again. So this arm sees an already-known death on every page
        // reload, and without a marker it filed a fresh `failed` and fired a banner for a
        // command that ended before the reload, every time, forever. The marker is
        // persisted with the events and pruned by `retain`.
        if (session.reportedExit) return undefined;
        session.reportedExit = true;
        // Keyed on `unseen` alone, and **not** on `ranCommand`. That was the bug: a
        // shell dying mid-command (a crash, `exit 3`, an OOM kill) has `ranCommand`
        // true and will never send its `D`, so the exit frame *is* the report — and
        // suppressing it produced no event at all for the one case that most needs one.
        // A session that already has an unread event has said what happened, and the
        // shell's own exit is not a second thing to badge.
        if (session.unseen) return undefined;
        return signal.code === 0
          ? {
              kind: "finished",
              producer: "command",
              at: now,
              source: "detected",
              detail: "Process ended",
            }
          : {
              kind: "failed",
              producer: "command",
              at: now,
              source: "detected",
              detail: midCommand
                ? `Ended while running a command (exit ${signal.code ?? "?"})`
                : `Process exited ${signal.code ?? "?"}`,
            };
      }

      case "notify":
        // The hook-less fallback, at the lowest authority there is. A notification only
        // ever means "notice me" — it cannot tell finished from waiting — so it raises
        // attention, and only where nothing better has spoken for this session.
        if (session.agentSource !== null) return undefined;
        return {
          kind: "attention",
          producer: "command",
          at: now,
          source: "detected",
          // Bounded: this is raw terminal output, and it now ends up in a system banner
          // *and* in `sessionStorage`. A program printing a few hundred KB into OSC 9
          // otherwise blew the storage quota, and that failure is swallowed — silently
          // disabling reload survival for the whole window.
          detail: bannerText(signal.message) || "Terminal activity",
        };

      case "agent": {
        if (signal.state === "unknown") return undefined;
        if (session.agentSource && !supersedes(signal.source, session.agentSource)) {
          return undefined;
        }
        // Authority is claimed only for a state this build understands. A newer daemon
        // sending one it does not (`compacting`, `paused`) used to reach here, set
        // `agentSource` and clear `agentWorking` before falling out of the switch — so an
        // unrecognised state *permanently muted* the shell's `working` and every OSC 9 in
        // that pane, rather than being the no-op the wire contract promises.
        if (!KNOWN_AGENT_STATES.includes(signal.state)) return undefined;
        session.agentSource = signal.source;
        session.agentWorking = signal.state === "working";
        // An outstanding `C` is this agent's own launch — remember it, so the `D` that
        // eventually closes it is recognised as the agent's rather than as fresh news.
        if (session.ranCommand) session.agentCommand = true;
        switch (signal.state) {
          case "ready":
            // Authority claimed, nothing reported. Setting `agentSource` above is the
            // whole payload: from here the shell's "a command is running" stops speaking
            // for this pane, which is what stops a freshly launched agent sitting idle at
            // its prompt from showing the activity spinner. Deliberately not an event —
            // `idle` would have put a spurious "agent finished" in the inbox on every
            // launch.
            return undefined;
          case "blocked":
            return {
              kind: "attention",
              producer: "agent",
              at: now,
              source: signal.source,
              detail: "Waiting for you",
            };
          case "done":
            session.agentWorking = false;
            // The claim goes with the agent's *command* where there is one — the `D` that
            // closes it releases the pane, and dropping the claim here instead would
            // disarm that guard for the mark arriving a moment later.
            //
            // Where there is **no** such command it has to be released now, because nothing
            // else ever will: an agent launched with `terminal.shellIntegration` off, or in
            // a config-declared pane (whose `-c` shell never prompts, so it emits no marks
            // at all), produces no `C` and no `D`. Leaving those claimed muted the pane's
            // activity for good and silently dropped every later OSC 9 in it.
            //
            // Safe precisely because it is conditional: with no agent-owned `C` outstanding,
            // any `D` that turns up belongs to some other command and has earned its event.
            if (!session.agentCommand) session.agentSource = null;
            return {
              kind: "finished",
              producer: "agent",
              at: now,
              source: signal.source,
              detail: "Agent session ended",
            };
          case "idle":
            return {
              kind: "finished",
              producer: "agent",
              at: now,
              source: signal.source,
              detail: "Agent finished",
            };
          case "working":
            // A retraction, not an event. The agent has moved on from whatever it was
            // waiting for — answered in another window, auto-approved, timed out — so an
            // `attention` still sitting there is a lie. Only an attention is retracted:
            // a `finished` the user has not read yet is still true.
            return session.unseen?.kind === "attention" ? null : undefined;
        }
      }
    }
  }

  /** Read a session's unseen event. Whether there was one. */
  read(sessionId: string): boolean {
    const session = this.sessions.get(sessionId);
    if (!session?.unseen) return false;
    session.unseen = null;
    this.notify();
    return true;
  }

  /**
   * A pane is starting a fresh process under the same session id.
   *
   * `restartTerminal` and `startTerminal` deliberately reuse the pane id — they delete the
   * daemon session and connect a new shell under the same name — so without this the
   * `reportedExit` marker survived the restart and **every exit after the first was
   * silently dropped**: no badge, no notification, not even a re-render. That is the
   * primary event path for a `oneshot` pane, which is the pane kind the exit producer
   * exists for.
   *
   * Explicit rather than inferred from a `ready` frame: a reattach to an already-dead
   * session also reports ready, and clearing the marker there would re-file the very exit
   * the marker exists to suppress.
   */
  restarted(sessionId: string): void {
    const session = this.sessions.get(sessionId);
    if (!session) return;
    session.reportedExit = false;
    session.ranCommand = false;
    session.agentWorking = false;
    session.agentSource = null;
    session.agentCommand = false;
    // The event goes too. Restart is reachable from an *inactive* tab's strip, so
    // restarting without having read is an ordinary gesture — and a badge left over from
    // the previous run then asserts a failure the current run has not had, with no fresh
    // event or timestamp when it succeeds or fails again.
    session.unseen = null;
    this.notify();
  }

  /**
   * Read everything in a worktree.
   *
   * The explicit gesture, and it clears the lot. There are no producer-held events a
   * user is not entitled to dismiss — every event here is a notification about
   * something that already happened, so "I have seen it" is always the user's to say.
   * When a producer that *holds* an event arrives (a share request that must be
   * answered rather than noticed), it will need its own acknowledgement mode; it must
   * not silently make this gesture a lie in the meantime.
   *
   * It does not, and must not, stop anything that is **running**: `working` is not an
   * event and there is nothing about it to have seen.
   */
  markWorktreeRead(worktreeId: number): void {
    let changed = false;
    for (const session of this.sessions.values()) {
      if (session.worktreeId === worktreeId && session.unseen) {
        session.unseen = null;
        changed = true;
      }
    }
    if (changed) this.notify();
  }

  /** Whether a worktree has anything a user could mark read. */
  hasUnread(worktreeId: number): boolean {
    for (const session of this.sessions.values()) {
      if (session.worktreeId === worktreeId && session.unseen) return true;
    }
    return false;
  }

  /**
   * What a worktree's rail row should show.
   *
   * One pass, because this runs per row on every render and a rail can hold every
   * worktree of a monorepo.
   */
  rowState(worktreeId: number, showWorking: boolean): RowSummary {
    const entries: { sessionId: string; unseen: Unseen }[] = [];
    let runningPanes = 0;
    const kinds = new Set<UnseenKind>();
    for (const [sessionId, session] of this.sessions) {
      if (session.worktreeId !== worktreeId) continue;
      if (running(session)) runningPanes += 1;
      if (!session.unseen) continue;
      entries.push({ sessionId, unseen: session.unseen });
      kinds.add(session.unseen.kind);
    }
    if (entries.length === 0 && !(showWorking && runningPanes > 0)) return NOTHING;
    entries.sort((a, b) => b.unseen.at - a.unseen.at);
    const state =
      PRECEDENCE.find(
        (candidate) =>
          candidate === "working"
            ? showWorking && runningPanes > 0
            : kinds.has(candidate as UnseenKind),
      ) ?? null;
    return { state, entries, running: runningPanes };
  }

  /** One session's unseen event, or null. For a pane's own tab. */
  unseen(sessionId: string): Unseen | null {
    return this.sessions.get(sessionId)?.unseen ?? null;
  }

  /** Whether a session is running something. For a pane's own tab. */
  isRunning(sessionId: string): boolean {
    const session = this.sessions.get(sessionId);
    return session ? running(session) : false;
  }

  /**
   * Forget a session entirely.
   *
   * Called when a pane is closed for good, not when it is unmounted: an unmounted pane's
   * events are still that worktree's news.
   */
  forget(sessionId: string): void {
    if (this.sessions.delete(sessionId)) this.notify();
  }

  /** Every session id the inbox knows. Tests and diagnostics. */
  known(): string[] {
    return [...this.sessions.keys()];
  }

  /**
   * The unread events, for persisting across a page reload.
   *
   * **Only the events.** `ranCommand`, `agentWorking` and `agentSource` are claims about
   * what a process is doing *right now*, and a reload is exactly the moment they stop
   * being knowable: the command may have finished while the page was gone. Restoring
   * `ranCommand: true` would show an activity spinner for something that is no longer
   * running, with nothing to ever clear it — the marks that would have are in the
   * scrollback, whose replay is deliberately suppressed. So live state is rebuilt from
   * signals after the reload, and only the inbox proper survives.
   */
  snapshot(): PersistedInbox {
    const sessions: PersistedInbox["sessions"] = {};
    for (const [id, session] of this.sessions) {
      if (session.unseen || session.reportedExit) {
        sessions[id] = {
          worktreeId: session.worktreeId,
          unseen: session.unseen,
          // Carried even for a session with nothing unread: it is the *read* case that
          // needed it, since the daemon replays the exit frame to the next attach and the
          // event would otherwise come back — with a notification — on every reload.
          reportedExit: session.reportedExit,
        };
      }
    }
    return { v: 1, sessions };
  }

  /**
   * Adopt a snapshot. Merges rather than replaces, so a restore that lands after a pane
   * has already reported something cannot undo it.
   *
   * Every field is checked: this is JSON from storage, which a user can edit, an older
   * build can have written, and a newer one can have extended.
   */
  restore(doc: unknown): void {
    const parsed = parsePersisted(doc);
    let changed = false;
    for (const [id, entry] of Object.entries(parsed)) {
      const existing = this.sessions.get(id);
      // A live event wins: it is newer by construction.
      if (existing?.unseen) continue;
      this.register(id, entry.worktreeId);
      const session = this.sessions.get(id);
      if (!session) continue;
      // Adopted even when there is no event to restore: it is what stops the daemon's
      // replayed exit frame filing the same death again after the reload.
      session.reportedExit = session.reportedExit || entry.reportedExit;
      if (entry.unseen !== null) {
        session.unseen = entry.unseen;
        changed = true;
      }
    }
    if (changed) this.notify();
  }

  /**
   * Drop sessions that their worktree's layout no longer names.
   *
   * The counterpart to a restore: a snapshot can name panes that were closed while the
   * page was reloading, and a restored event for a pane that no longer exists is one the
   * user can never read by looking — the poisoned badge the design set out to avoid.
   *
   * # `withinWorktrees` is the whole correctness of this function
   *
   * It prunes **only** worktrees whose layout the caller actually has, and that is not a
   * refinement — without it this deleted the entire restored inbox on every reload.
   * `readLayouts` is explicit that *a main window gets nothing from storage*: its layouts
   * come from the daemon, asynchronously, one worktree at a time. So the first render
   * after a reload has `layouts === {}`, and a version that pruned everything not in that
   * empty set threw away exactly what had just been restored — a guard against phantom
   * badges that destroyed the feature it was guarding.
   *
   * "I have no layout for this worktree" and "this worktree has no panes" are different
   * facts, and only the caller can tell them apart.
   */
  retain(keep: Iterable<string>, withinWorktrees: Iterable<number>): void {
    const live = new Set(keep);
    const known = new Set(withinWorktrees);
    let changed = false;
    for (const [id, session] of [...this.sessions]) {
      if (!known.has(session.worktreeId)) continue;
      if (!live.has(id) && this.sessions.delete(id)) changed = true;
    }
    if (changed) this.notify();
  }
}

/** The shape written to storage. Versioned, so a future change can migrate or discard. */
export interface PersistedInbox {
  v: 1;
  sessions: Record<
    string,
    {
      worktreeId: number;
      /** `null` for a session kept only to remember that its exit was already filed. */
      unseen: Unseen | null;
      reportedExit: boolean;
    }
  >;
}

const KINDS: UnseenKind[] = ["finished", "failed", "attention"];
const PRODUCERS: Producer[] = ["command", "agent"];
const SOURCES: Source[] = ["detected", "socket", "hook"];

/**
 * The sessions in a stored document, dropping anything that does not typecheck.
 *
 * Lenient per entry rather than all-or-nothing: one bad row from an older build should
 * cost that row, not the whole inbox.
 */
function parsePersisted(doc: unknown): PersistedInbox["sessions"] {
  const out: PersistedInbox["sessions"] = {};
  if (typeof doc !== "object" || doc === null) return out;
  const sessions = (doc as { sessions?: unknown }).sessions;
  if (typeof sessions !== "object" || sessions === null) return out;
  for (const [id, raw] of Object.entries(sessions as Record<string, unknown>)) {
    if (typeof raw !== "object" || raw === null) continue;
    const entry = raw as Record<string, unknown>;
    const unseen = entry.unseen;
    if (typeof entry.worktreeId !== "number") continue;
    const reportedExit = entry.reportedExit === true;
    // A row with no event but a filed exit is meaningful on its own — it is the whole
    // point of persisting the marker — so it is kept rather than skipped.
    if (unseen === null || unseen === undefined) {
      if (reportedExit) {
        out[id] = { worktreeId: entry.worktreeId, unseen: null, reportedExit };
      }
      continue;
    }
    if (typeof unseen !== "object") continue;
    const u = unseen as Record<string, unknown>;
    if (!KINDS.includes(u.kind as UnseenKind)) continue;
    if (!PRODUCERS.includes(u.producer as Producer)) continue;
    if (!SOURCES.includes(u.source as Source)) continue;
    if (typeof u.at !== "number" || typeof u.detail !== "string") continue;
    out[id] = {
      worktreeId: entry.worktreeId,
      reportedExit,
      unseen: {
        kind: u.kind as UnseenKind,
        producer: u.producer as Producer,
        source: u.source as Source,
        at: u.at,
        detail: u.detail,
      },
    };
  }
  return out;
}

export const inbox = new WorktreeInbox();

/** A fresh, isolated inbox. Tests only — the app has exactly one. */
export function createInbox(): WorktreeInbox {
  return new WorktreeInbox();
}

export type { WorktreeInbox };

/**
 * The OSC 133 marks in a chunk of terminal output.
 *
 * A scan and not a parse: xterm is already parsing these bytes and hands OSC payloads
 * to a registered handler, so this exists for the payload *shape* — `D;1` versus `D` —
 * rather than to find the sequences. Kept pure so the fixtures can drive it.
 */
export function parseOsc133(payload: string): Signal | null {
  const [mark, status] = payload.split(";", 2);
  switch (mark) {
    case "A":
    case "B":
    case "C":
      return { type: "osc133", mark, exit: null };
    case "D": {
      // `D` alone means "ended, status unstated". Treated as success: a shell that
      // omits the status is not reporting a failure, and inventing one would badge
      // `failed` on every command in a terminal whose integration is half-configured.
      if (status === undefined || status === "") {
        return { type: "osc133", mark: "D", exit: 0 };
      }
      const exit = Number.parseInt(status, 10);
      return {
        type: "osc133",
        mark: "D",
        exit: Number.isNaN(exit) ? null : exit,
      };
    }
    default:
      return null;
  }
}

/**
 * Whether an OSC 9 payload is a notification rather than a progress report.
 *
 * **`OSC 9;4` is the ConEmu/Windows-Terminal progress sequence, not a notification** —
 * `ESC ] 9 ; 4 ; <state> ; <percent> BEL` — and Claude Code emits it. Handing it to a
 * notification consumer produces a banner whose text is `4;1;50`, and wiring that to the
 * inbox would raise `attention` on every progress tick of the exact tool this feature
 * exists to watch. The test is a whole numeric *field*, not merely a leading digit,
 * because "42 tests passed" is a perfectly good notification.
 */
export function isOsc9Notification(payload: string): boolean {
  return !/^\d+(;|$)/.test(payload);
}

/**
 * Which notification setting governs an event.
 *
 * Four rows rather than one switch, because "a command finished" and "a coding agent
 * finished" are not the same event. `attention` maps to one row whatever produced it:
 * an OSC 9 "notice me" from a plain program is the same claim on the user's attention
 * as a blocked agent, which is also why that row is not called "agent waiting".
 */
export function notifyKey(unseen: Unseen): string {
  if (unseen.kind === "attention") {
    // Two rows, not one, and the producer is what splits them: an agent stopped at a
    // permission prompt and a program that emitted OSC 9 are both "notice me", but a
    // row labelled for coding agents must not silently fire for `curl`. Folding them
    // together is what made the label lie about half of what it did.
    return unseen.producer === "agent"
      ? "activity.notifyAgentWaiting"
      : "activity.notifyNoticed";
  }
  if (unseen.producer === "agent") return "activity.notifyAgentFinished";
  return unseen.kind === "failed"
    ? "activity.notifyCommandFailed"
    : "activity.notifyCommandFinished";
}

/**
 * Typing a queued prompt into a pane whose agent did not take it at launch.
 *
 * The sequence itself, [`deliverQueuedPrompt`], without the terminal it runs
 * against; `terminalHost`'s `armInitialPrompt` wires it to a session. It lives
 * here rather than in `terminalHost` for the reason [`promptStep`] lives in
 * `model.ts`: `terminalHost` cannot be imported without a DOM and a live xterm
 * `Terminal`, so the only way to test the order of writes, the waits
 * between them and the toasts at the end is to hand all of it in. Everything the
 * sequence touches is a port on [`PromptPorts`]; `terminalHost` passes the real
 * ones and `promptDelivery.test.ts` passes fakes.
 */

import { type PromptStep, promptStep } from "./model";
import { echoed, isImagePath, pasteLanded, pathPayload, promptPastes, type WriteSource } from "./terminalPaste";

/** How often the delivery gate is re-checked while an agent starts up. */
export const PROMPT_POLL_MS = 120;

/**
 * How long to wait for a pane's program to open an input before giving up.
 *
 * Generous, because the wait is invisible when it succeeds and the cost of it
 * being too short is a prompt silently not sent: an agent's first run in a fresh
 * checkout can spend several seconds on a version check or an auth refresh
 * before it draws anything.
 */
export const PROMPT_WAIT_MS = 25_000;

/**
 * Pause between the gate opening and the first paste.
 *
 * `bracketedPasteMode` flips when xterm *parses* the escape, which is when the
 * program asked for it — a beat before its input loop is necessarily reading.
 * Not load-bearing for correctness; it is what makes the common case work on
 * the first try rather than on the retry the user has to notice.
 */
export const PROMPT_SETTLE_MS = 300;

/**
 * How a paste is known to have landed before the next write: the program's
 * redraw, then this long without output (see [`echoed`]).
 *
 * Every paste waits, not only the last before the `\r`: each attachment is its
 * own paste so a composer recognises it on its own, and an image path still
 * being read when the next path arrives would have its placeholder land after
 * it — out of order, or in the next message.
 */
export const PROMPT_ECHO_QUIET_MS = 250;

/**
 * The longest a paste waits for its redraw before the next write goes anyway.
 *
 * A paste that never shows at all still gets its `\r`, which is what the fixed
 * pause used to do.
 *
 * **Measured, and left alone.** The cap is also what an agent that never goes
 * quiet waits on every paste, so it was timed before anything changed — by
 * hand, once, with temporary logging around each `echoed` call that was not
 * kept (`dev-headless`, a new worktree's prompt plus one text and one image
 * attachment, 2026-10):
 *
 * - Claude Code 2.1.294 with `terminal.agentIntegration` off, three runs: text
 *   pastes answered in 258–647 ms, image pastes in 274–491 ms. Never the cap.
 * - A synthetic worst case — bracketed paste on, a spinner line redrawn every
 *   ~100 ms, the composer echoed — two runs: text pastes 1.3–3.9 s, and one of
 *   four hit this cap; both image pastes ran to [`PROMPT_IMAGE_CAP_MS`] even
 *   though the name was on screen, because the quiet never comes.
 *
 * Codex and Pi were not measured (not installed). If either redraws while idle,
 * the fix is to end the wait when the paste's own tail is on screen, keeping
 * quiet-or-cap as the fallback — not a lower cap, which would cut short the
 * agent that is merely slow.
 */
export const PROMPT_ECHO_CAP_MS = 5_000;

/**
 * The same for an image path, which waits for its placeholder rather than for
 * any redraw (see [`pasteLanded`]). Longer, because the agent decodes, resizes
 * and re-encodes the picture first, and a full-resolution Retina screenshot is
 * the common case. An agent that shows neither a placeholder nor the name
 * waits this long — and so does one that never stops redrawing, since the
 * quiet after the placeholder never comes (see [`PROMPT_ECHO_CAP_MS`]).
 */
export const PROMPT_IMAGE_CAP_MS = 15_000;

/** What the sequence needs of a queued prompt. `terminalHost`'s `QueuedPrompt`
 *  is one; structural, so this module does not import the one that imports it. */
export interface DeliverablePrompt {
  text: string;
  label: string;
  files: readonly { file: { name: string } }[];
  /** True once a launch was handed the prompt: the window then types it only if
   *  the claim says the agent's wrapper left it. */
  atLaunch?: boolean;
  /** The files' paths if a launch already resolved them, so they are not
   *  uploaded a second time. */
  paths?: readonly string[];
}

/** The slice of a terminal the sequence writes to and watches. */
export interface PromptTerminal extends WriteSource {
  /** `Terminal.paste` — bracketed while the program has the mode on. */
  paste(text: string): void;
  /** `Terminal.input` — the route a keystroke takes. */
  input(data: string): void;
  /** The bottom of the screen as text, for [`pasteLanded`]. */
  screen(): string;
}

/** Everything the sequence touches, passed in. */
export interface PromptPorts {
  /** Whether this prompt is still queued — another armer may have taken it. */
  queued(): boolean;
  /** Forget the queued prompt, so no other armer delivers it. */
  dequeue(): void;
  /** Whether the session is still the one this was armed for; false once it
   *  restarted. The one input [`promptStep`] cannot see, because it is a fact
   *  about this armer rather than about the terminal. */
  current(): boolean;
  /** The terminal facts [`promptStep`] decides on, read at this instant. The
   *  deadline is this module's, so `expired` is not one of them. */
  gate(): Omit<Parameters<typeof promptStep>[0], "expired">;
  /** Take the launch prompt back from the daemon: true when the agent's wrapper
   *  left it, false when the agent took it, null when the daemon has no record
   *  of it. A throw counts as null too. */
  claim(): Promise<boolean | null>;
  /** Resolve the files to paths, uploading a browser tab's. */
  resolve(): Promise<string[]>;
  term: PromptTerminal;
  /** Raise the "not sent" toast with `detail` as its body. */
  fail(title: string, detail: string): void;
  clock: {
    now(): number;
    setTimeout(fn: () => void, ms: number): void;
  };
}

/**
 * Hand a pane its queued prompt once its program is reading input, or say why it
 * never got it.
 *
 * **The gate is `bracketedPasteMode`, and everything about this function is
 * downstream of that choice.** Writing the text after a fixed delay was the
 * obvious version and it is the dangerous one: a pane is
 * `<shell> -l -i -c '<command>'`, so anything written before the command has
 * taken the terminal over is read by whatever *is* there — and a newline after
 * it is an instruction to run it. DECSET 2004 is the narrowest available proof
 * that a full-screen input program has the keyboard: `-c` means the wrapping
 * shell never starts its line editor, so it cannot be the thing that set the
 * mode, and every agent TUI this exists for (Claude Code, Codex) sets it.
 *
 * Three further conditions, each guarding a real failure:
 *
 * - **`spec !== undefined`** — config-declared panes only. A plain terminal
 *   *is* an interactive shell with bracketed paste on, so the gate says nothing
 *   there and the newline would run the prompt as a command. Nothing queues a
 *   prompt against a plain terminal today; this is what keeps that true.
 * - **`!replaying`** — xterm sets the mode while parsing *replayed*
 *   scrollback, so a reattach can show the flag for a program that set it
 *   before the page was reloaded. `terminalHost`'s `canSend` already refuses
 *   to send during a replay; reading the mode during one would pick the wrong moment rather than
 *   the wrong shell.
 * - **the entry is dequeued before the paste is scheduled** — two connects (a
 *   drop and its auto-reconnect) can both arm this, and the second sees the
 *   entry gone. A prompt delivered twice is a second turn the user never asked
 *   for, which for an agent means real work done twice.
 *
 * A pane whose program never opens an input keeps its prompt un-sent, and the
 * text comes back on a toast rather than being written anyway: it is the case
 * where the gate has told us we do not know what is reading, and the whole
 * reason for the gate is not to type into that.
 */
export function deliverQueuedPrompt(queued: DeliverablePrompt, ports: PromptPorts): void {
  const { text, label, files } = queued;
  const { term, clock } = ports;
  const deadline = clock.now() + PROMPT_WAIT_MS;
  const step = (): PromptStep => promptStep({ ...ports.gate(), expired: clock.now() >= deadline });
  const sendable = () => step() === "send" && ports.current();
  const pause = (ms: number) => new Promise<void>((done) => clock.setTimeout(done, ms));
  const tick = () => {
    // Restarted: whatever is there now was not opened for this prompt.
    if (!ports.current()) {
      ports.dequeue();
      return;
    }
    // Another armer got there first.
    if (!ports.queued()) return;
    const now = step();
    if (now === "no-pane" || now === "give-up") {
      ports.dequeue();
      if (now === "no-pane") return;
      void reportUnlessSent();
      return;
    }
    if (now === "send") {
      ports.dequeue();
      void deliver();
      return;
    }
    if (now === "expired") {
      ports.dequeue();
      void reportUnlessSent();
      return;
    }
    clock.setTimeout(tick, PROMPT_POLL_MS);
  };
  /**
   * Say the prompt was not sent, and hand it back.
   *
   * It travels on the toast because nothing was written into the pane and the
   * user should not have to retype what they already said.
   */
  const report = (title = `Your prompt was not sent — the ${label} pane never opened one`) => {
    // The attachments are named too: a browser tab's are bytes held in this
    // page, so the names are all that is left to re-attach them by.
    const attached = files.length > 0 ? `Attached: ${files.map((f) => f.file.name).join(", ")}` : "";
    ports.fail(title, [text, attached].filter((part) => part !== "").join("\n\n"));
  };
  /**
   * Whether this window still owes the prompt: always, unless a launch was handed
   * it — then only if the claim says the agent's wrapper left it.
   *
   * Asked once the agent's input is up (or never will be), which is after its
   * wrapper ran, so the answer is final. `null` when the daemon could not say —
   * unreachable, or restarted since the launch and holding no record of it:
   * the agent may or may not have it, and neither typing it (twice?) nor saying
   * nothing (lost?) is safe, so the caller hands it back and says so.
   */
  const owed = async (): Promise<boolean | null> => {
    if (!queued.atLaunch) return true;
    try {
      return await ports.claim();
    } catch (e) {
      console.warn("veld: could not claim the launch prompt", e);
      return null;
    }
  };
  const unsure = () => report(`Could not tell whether ${label} got your prompt — check the pane before resending`);
  /**
   * The pane never opened an input. Owed, it was never sent; taken by the
   * wrapper, the agent had it and then stopped before showing anything — an
   * auth or update exit, a flag `--` did not suit — so it may have gone down
   * with the agent. Neither is a case to stay quiet about.
   */
  const reportUnlessSent = async () => {
    const mine = await owed();
    if (mine === true) report();
    else unsure();
  };
  /**
   * Paste the prompt, then each attachment's path, then submit — in the order
   * `promptPastes` gives, which owns why.
   *
   * **Every gap re-reads the gate**, for the reason the single paste always did
   * (see the note on `Terminal.paste` below): the program can leave or drop
   * bracketed paste between any two writes, and a path or a `\r` written into
   * whatever replaced it is keystrokes nobody asked for.
   *
   * A browser tab's files are uploaded here, against this session — the first
   * moment one exists — and the uploads overlap the settle pause rather than
   * adding to it. One that fails costs only itself: the prompt still goes, with
   * the rest, and the toast names what is missing.
   */
  const deliver = async () => {
    const settled = pause(PROMPT_SETTLE_MS);
    // The agent opened with it already sent: nothing to type.
    const mine = await owed();
    if (mine === null) return unsure();
    if (!mine) return;
    // Already resolved when the launch was offered it, so not uploaded twice.
    const paths = queued.paths ?? (await ports.resolve());
    await settled;
    const pastes = promptPastes(text, paths);
    if (pastes.length === 0) return;
    // **Re-read the gate, do not trust the tick that scheduled this.** The
    // mode is a mutable terminal state, and `Terminal.paste` reads it at
    // call time: it wraps the text in `ESC[200~`/`ESC[201~` only while the
    // mode is on, and rewrites every `\n` to `\r` either way (measured in
    // the installed build — `xterm.js` module 3614). The claim, the uploads
    // and the settle all await before this, and every paste below waits on
    // its echo, so seconds can pass between reads. A program that cleared
    // DECSET 2004 in that time — exiting, or a nested reader that sets and
    // resets it per line — would turn a multi-line prompt into one bare
    // `\r`-terminated line per line of it, each submitting itself. That is
    // the exact outcome pasting rather than typing exists to prevent.
    if (!sendable()) return;
    // `paste`, not `input`, for the reason `terminalHost`'s file drop uses it:
    // an agent reads a bracketed paste as one block, so a multi-line prompt
    // arrives as one message instead of as a line that submits itself at every
    // newline.
    //
    // Each paste waits for the program to show it before anything else is
    // written — the `\r` included, which sent too early submits the message
    // without an image the agent was still reading (see [`echoed`]).
    const images = new Map(paths.filter(isImagePath).map((p) => [pathPayload([p]), p]));
    for (const paste of pastes) {
      const image = images.get(paste);
      const before = image === undefined ? "" : term.screen();
      const shown =
        image === undefined
          ? echoed(term, PROMPT_ECHO_QUIET_MS, PROMPT_ECHO_CAP_MS)
          : echoed(term, PROMPT_ECHO_QUIET_MS, PROMPT_IMAGE_CAP_MS, () => pasteLanded(before, term.screen(), image));
      term.paste(paste);
      await shown;
      // Re-read after every wait: the program can have gone in it, and a
      // path or a `\r` into whatever replaced it is keystrokes nobody asked
      // for.
      if (!sendable()) return;
    }
    // `\r`, the byte Return sends. Through `input` so it takes the same route
    // as a keystroke — including marking the pane read, which is honest: the
    // user is the reason something was just typed here.
    term.input("\r");
  };
  tick();
}

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  type DeliverablePrompt,
  deliverQueuedPrompt,
  PROMPT_ECHO_CAP_MS,
  PROMPT_ECHO_QUIET_MS,
  PROMPT_IMAGE_CAP_MS,
  PROMPT_POLL_MS,
  PROMPT_SETTLE_MS,
  PROMPT_WAIT_MS,
  type PromptPorts,
} from "./promptDelivery";

/**
 * A pane, as far as the delivery sequence can see one.
 *
 * `log` records what the sequence did to the terminal, in order — `listen` when
 * it starts waiting for output, `paste:…` and `input:…` for what it wrote — so a
 * test can assert the order of writes and waits rather than only their sum.
 * `onPaste` is the program on the other end: what it draws in answer to a paste.
 */
function pane(prompt: Partial<DeliverablePrompt> = {}) {
  const listeners = new Set<() => void>();
  const log: string[] = [];
  let screen = "> ";
  const state = {
    queued: true,
    current: true,
    gate: {
      spec: "claude" as string | undefined,
      registered: true,
      wsOpen: true,
      replaying: false,
      bracketedPaste: true,
      ended: false,
    },
    onPaste: (text: string) => draw(text),
  };
  /** The program writes `text` to the screen; xterm parses it. */
  const draw = (text = "") => {
    screen += text;
    for (const l of [...listeners]) l();
  };
  const ports = {
    queued: () => state.queued,
    dequeue: vi.fn(() => {
      state.queued = false;
    }),
    current: () => state.current,
    gate: () => ({ ...state.gate }),
    claim: vi.fn<PromptPorts["claim"]>(async () => true),
    resolve: vi.fn<PromptPorts["resolve"]>(async () => []),
    term: {
      paste: (text: string) => {
        log.push(`paste:${text}`);
        state.onPaste(text);
      },
      input: (data: string) => {
        log.push(`input:${data}`);
      },
      screen: () => screen,
      onWriteParsed: (listener: () => void) => {
        log.push("listen");
        listeners.add(listener);
        return { dispose: () => listeners.delete(listener) };
      },
    },
    fail: vi.fn<PromptPorts["fail"]>(),
    clock: { now: () => Date.now(), setTimeout: (fn: () => void, ms: number) => void setTimeout(fn, ms) },
  } satisfies PromptPorts;
  const queued: DeliverablePrompt = { text: "fix the bug", label: "Claude", files: [], ...prompt };
  return {
    state,
    ports,
    log,
    draw,
    /** What reached the terminal, without the waits. */
    writes: () => log.filter((e) => e !== "listen"),
    arm: () => deliverQueuedPrompt(queued, ports),
  };
}

const files = (...names: string[]) => names.map((name) => ({ file: { name } }));

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("whose prompt it is", () => {
  it("types nothing and says nothing when the agent's wrapper took it at launch", async () => {
    const p = pane({ atLaunch: true });
    p.ports.claim.mockResolvedValue(false);
    p.arm();
    await vi.runAllTimersAsync();
    expect(p.ports.claim).toHaveBeenCalledTimes(1);
    expect(p.writes()).toEqual([]);
    expect(p.ports.fail).not.toHaveBeenCalled();
  });

  it("types nothing and says it could not tell when the daemon has no record", async () => {
    const p = pane({ atLaunch: true });
    p.ports.claim.mockResolvedValue(null);
    p.arm();
    await vi.runAllTimersAsync();
    expect(p.writes()).toEqual([]);
    expect(p.ports.fail).toHaveBeenCalledTimes(1);
    expect(p.ports.fail.mock.calls[0][0]).toMatch(/^Could not tell whether Claude got your prompt/);
  });

  it("treats an unreachable daemon as one that could not tell", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    const p = pane({ atLaunch: true });
    p.ports.claim.mockRejectedValue(new Error("connection refused"));
    p.arm();
    await vi.runAllTimersAsync();
    expect(p.writes()).toEqual([]);
    expect(p.ports.fail.mock.calls[0][0]).toMatch(/^Could not tell/);
  });

  it("does not ask the daemon about a prompt no launch was handed", async () => {
    const p = pane();
    p.arm();
    await vi.runAllTimersAsync();
    expect(p.ports.claim).not.toHaveBeenCalled();
    expect(p.writes()).toEqual(["paste:fix the bug", "input:\r"]);
  });

  it("types a prompt the wrapper left", async () => {
    const p = pane({ atLaunch: true });
    p.arm();
    await vi.runAllTimersAsync();
    expect(p.writes()).toEqual(["paste:fix the bug", "input:\r"]);
  });
});

describe("the order of writes", () => {
  it("is the prompt, then each attachment as a paste of its own, then Return", async () => {
    const p = pane({ files: files("notes.txt", "My Plan.md") });
    p.ports.resolve.mockResolvedValue(["/tmp/notes.txt", "/tmp/My Plan.md"]);
    p.arm();
    await vi.runAllTimersAsync();
    expect(p.writes()).toEqual([
      "paste:fix the bug\n\n",
      "paste:/tmp/notes.txt ",
      "paste:/tmp/My\\ Plan.md ",
      "input:\r",
    ]);
  });

  it("starts listening for each paste's echo before it pastes", async () => {
    // The program in this test answers *inside* the paste call — the fastest an
    // echo can come. A wait armed after the paste misses it and runs to the cap.
    const p = pane({ files: files("a.txt") });
    p.ports.resolve.mockResolvedValue(["/tmp/a.txt"]);
    const start = Date.now();
    p.arm();
    await vi.runAllTimersAsync();
    expect(p.log).toEqual(["listen", "paste:fix the bug\n\n", "listen", "paste:/tmp/a.txt ", "input:\r"]);
    expect(Date.now() - start).toBe(PROMPT_SETTLE_MS + 2 * PROMPT_ECHO_QUIET_MS);
  });

  it("uses the paths a launch already resolved, rather than uploading again", async () => {
    const p = pane({ atLaunch: true, files: files("a.txt"), paths: ["/tmp/a.txt"] });
    p.arm();
    await vi.runAllTimersAsync();
    expect(p.ports.resolve).not.toHaveBeenCalled();
    expect(p.writes()).toContain("paste:/tmp/a.txt ");
  });

  it("waits out the settle pause before the first paste", async () => {
    const p = pane();
    p.arm();
    await vi.advanceTimersByTimeAsync(PROMPT_SETTLE_MS - 1);
    expect(p.writes()).toEqual([]);
    await vi.advanceTimersByTimeAsync(1);
    expect(p.writes()).toEqual(["paste:fix the bug"]);
  });

  it("waits for the gate to open before it does anything", async () => {
    const p = pane();
    p.state.gate.bracketedPaste = false;
    p.arm();
    await vi.advanceTimersByTimeAsync(5 * PROMPT_POLL_MS);
    expect(p.ports.dequeue).not.toHaveBeenCalled();
    p.state.gate.bracketedPaste = true;
    await vi.runAllTimersAsync();
    expect(p.writes()).toEqual(["paste:fix the bug", "input:\r"]);
  });
});

describe("the gate closing mid-sequence", () => {
  // Three pastes, so four waits: the settle, and one echo after each paste.
  // Whichever one the program leaves bracketed paste in, nothing more is written.
  const pastes = ["fix the bug\n\n", "/tmp/a.txt ", "/tmp/b.txt "];

  it.each([0, 1, 2, 3])("writes nothing after wait %i, and no Return", async (wait) => {
    const p = pane({ files: files("a.txt", "b.txt") });
    p.ports.resolve.mockResolvedValue(["/tmp/a.txt", "/tmp/b.txt"]);
    let seen = 0;
    p.state.onPaste = (text) => {
      seen += 1;
      if (seen === wait) p.state.gate.bracketedPaste = false;
      p.draw(text);
    };
    p.arm();
    if (wait === 0) {
      await vi.advanceTimersByTimeAsync(PROMPT_SETTLE_MS - 1);
      p.state.gate.bracketedPaste = false;
    }
    await vi.runAllTimersAsync();
    expect(p.writes()).toEqual(pastes.slice(0, wait).map((t) => `paste:${t}`));
  });

  it("writes nothing more once the pane restarts mid-sequence", async () => {
    const p = pane({ files: files("a.txt") });
    p.ports.resolve.mockResolvedValue(["/tmp/a.txt"]);
    p.state.onPaste = (text) => {
      p.state.current = false;
      p.draw(text);
    };
    p.arm();
    await vi.runAllTimersAsync();
    expect(p.writes()).toEqual(["paste:fix the bug\n\n"]);
  });
});

describe("how long each paste waits", () => {
  it("a text paste waits for its echo to go quiet", async () => {
    const p = pane();
    p.state.onPaste = () => {};
    p.arm();
    await vi.advanceTimersByTimeAsync(PROMPT_SETTLE_MS + 1_000);
    expect(p.writes()).toEqual(["paste:fix the bug"]);
    // The echo arrives in several writes; each restarts the quiet window.
    p.draw("fix the");
    await vi.advanceTimersByTimeAsync(PROMPT_ECHO_QUIET_MS - 50);
    p.draw(" bug");
    await vi.advanceTimersByTimeAsync(PROMPT_ECHO_QUIET_MS - 1);
    expect(p.writes()).not.toContain("input:\r");
    await vi.advanceTimersByTimeAsync(1);
    expect(p.writes()).toContain("input:\r");
  });

  it("a text paste nothing answers goes on at the cap", async () => {
    const p = pane();
    p.state.onPaste = () => {};
    p.arm();
    await vi.advanceTimersByTimeAsync(PROMPT_SETTLE_MS + PROMPT_ECHO_CAP_MS - 1);
    expect(p.writes()).not.toContain("input:\r");
    await vi.advanceTimersByTimeAsync(1);
    expect(p.writes()).toContain("input:\r");
  });

  it("an image path waits for its placeholder, not for the first redraw", async () => {
    const p = pane({ text: "", files: files("shot.png") });
    p.ports.resolve.mockResolvedValue(["/tmp/pastes/e22a-shot.png"]);
    // The agent echoes nothing that counts while it reads the picture.
    p.state.onPaste = () => p.draw("\n  Pasting…");
    p.arm();
    await vi.advanceTimersByTimeAsync(PROMPT_SETTLE_MS + 3_000);
    expect(p.writes()).toEqual(["paste:/tmp/pastes/e22a-shot.png "]);
    p.draw("\n> [Image #1] ");
    await vi.advanceTimersByTimeAsync(PROMPT_ECHO_QUIET_MS);
    expect(p.writes()).toEqual(["paste:/tmp/pastes/e22a-shot.png ", "input:\r"]);
  });

  it("an image path an agent never answers goes on at the image cap", async () => {
    const p = pane({ text: "", files: files("shot.png") });
    p.ports.resolve.mockResolvedValue(["/tmp/shot.png"]);
    p.state.onPaste = () => p.draw("\n  Pasting…");
    p.arm();
    await vi.advanceTimersByTimeAsync(PROMPT_SETTLE_MS + PROMPT_IMAGE_CAP_MS - 1);
    expect(p.writes()).not.toContain("input:\r");
    await vi.advanceTimersByTimeAsync(1);
    expect(p.writes()).toContain("input:\r");
  });
});

describe("a prompt that is never typed", () => {
  it("comes back on the toast, with the names of its attachments, when the gate never opens", async () => {
    const p = pane({ files: files("shot.png", "notes.txt") });
    p.state.gate.bracketedPaste = false;
    p.arm();
    await vi.advanceTimersByTimeAsync(PROMPT_WAIT_MS - PROMPT_POLL_MS);
    expect(p.ports.fail).not.toHaveBeenCalled();
    // Within a poll or two of the deadline, not merely at some point.
    await vi.advanceTimersByTimeAsync(2 * PROMPT_POLL_MS);
    expect(p.writes()).toEqual([]);
    expect(p.ports.fail).toHaveBeenCalledWith(
      "Your prompt was not sent — the Claude pane never opened one",
      "fix the bug\n\nAttached: shot.png, notes.txt",
    );
  });

  it("comes back the same way when the program exits first", async () => {
    const p = pane();
    p.state.gate.bracketedPaste = false;
    p.state.gate.ended = true;
    p.arm();
    await vi.runAllTimersAsync();
    expect(p.ports.fail).toHaveBeenCalledWith(
      "Your prompt was not sent — the Claude pane never opened one",
      "fix the bug",
    );
  });

  it("comes back when a launch was handed it and the wrapper left it", async () => {
    const p = pane({ atLaunch: true });
    p.state.gate.ended = true;
    p.state.gate.bracketedPaste = false;
    p.arm();
    await vi.runAllTimersAsync();
    expect(p.ports.fail.mock.calls[0][0]).toMatch(/^Your prompt was not sent/);
  });

  it.each([false, null])("says it could not tell when the claim answers %s", async (answer) => {
    // `false`: the wrapper took it, and the agent stopped before showing an
    // input — it may have gone down with the agent.
    const p = pane({ atLaunch: true });
    p.ports.claim.mockResolvedValue(answer);
    p.state.gate.bracketedPaste = false;
    p.arm();
    await vi.runAllTimersAsync();
    expect(p.ports.fail).toHaveBeenCalledTimes(1);
    expect(p.ports.fail.mock.calls[0][0]).toBe(
      "Could not tell whether Claude got your prompt — check the pane before resending",
    );
  });

  it("is dropped without a word once the pane restarts", async () => {
    const p = pane();
    p.state.gate.bracketedPaste = false;
    p.arm();
    await vi.advanceTimersByTimeAsync(PROMPT_POLL_MS);
    p.state.current = false;
    p.state.gate.bracketedPaste = true;
    await vi.runAllTimersAsync();
    expect(p.ports.dequeue).toHaveBeenCalled();
    expect(p.writes()).toEqual([]);
    expect(p.ports.fail).not.toHaveBeenCalled();
  });

  it("is left alone when another connect already took it", async () => {
    const p = pane();
    p.state.queued = false;
    p.arm();
    await vi.runAllTimersAsync();
    expect(p.writes()).toEqual([]);
    expect(p.ports.fail).not.toHaveBeenCalled();
  });

  it("is dropped without a word for a pane that is not a configured one", async () => {
    const p = pane();
    p.state.gate.spec = undefined;
    p.arm();
    await vi.runAllTimersAsync();
    expect(p.ports.dequeue).toHaveBeenCalled();
    expect(p.writes()).toEqual([]);
    expect(p.ports.fail).not.toHaveBeenCalled();
  });
});

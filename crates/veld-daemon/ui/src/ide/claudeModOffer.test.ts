import { describe, expect, it } from "vitest";

import {
  askBeforeClaude,
  asksBeforeStarting,
  CLAUDE_MOD_OFFER_ID,
  setClaudeModAsker,
  wantsClaudeModOffer,
} from "./claudeModOffer";

describe("the Claude mod offer", () => {
  it("is asked by a Claude pane about to start, and by nothing else", () => {
    expect(asksBeforeStarting({ agent_tool: "claude" }, false)).toBe(true);
    // A session already running keeps whatever it was started with.
    expect(asksBeforeStarting({ agent_tool: "claude" }, true)).toBe(false);
    expect(asksBeforeStarting({ agent_tool: "codex" }, false)).toBe(false);
    expect(asksBeforeStarting({}, false)).toBe(false);
    expect(asksBeforeStarting(undefined, false)).toBe(false);
  });

  it("is made once, to somebody on the wrapper with an agent switch on", () => {
    const wrapper = { "terminal.claudeIntegration": "wrapper" };
    expect(wantsClaudeModOffer(wrapper, {})).toBe(true);
    // A daemon too old to know the setting reads as the wrapper, which it is.
    expect(wantsClaudeModOffer({}, {})).toBe(true);
    // Any answer at all ends it — closing the dialog included.
    expect(wantsClaudeModOffer(wrapper, { [CLAUDE_MOD_OFFER_ID]: "dismissed" })).toBe(false);
    expect(wantsClaudeModOffer(wrapper, { [CLAUDE_MOD_OFFER_ID]: "read" })).toBe(false);
    expect(wantsClaudeModOffer({ "terminal.claudeIntegration": "mod" }, {})).toBe(false);
    expect(
      wantsClaudeModOffer(
        { ...wrapper, "terminal.agentIntegration": false, "terminal.agentContext": false },
        {},
      ),
    ).toBe(false);
    expect(wantsClaudeModOffer({ ...wrapper, "terminal.agentIntegration": false }, {})).toBe(true);
    // Settings not loaded yet is never a yes.
    expect(wantsClaudeModOffer(null, {})).toBe(false);
  });

  it("holds every pane on one question, and never holds one back for good", async () => {
    // Nobody installed to ask: start at once.
    await expect(askBeforeClaude()).resolves.toBeUndefined();

    let calls = 0;
    let answer: () => void = () => {};
    const uninstall = setClaudeModAsker(() => {
      calls += 1;
      return new Promise<void>((resolve) => {
        answer = resolve;
      });
    });
    const first = askBeforeClaude();
    const second = askBeforeClaude();
    expect(calls).toBe(1);
    answer();
    await Promise.all([first, second]);

    // A failed ask lets the pane start rather than leaving it blank.
    uninstall();
    const uninstallFailing = setClaudeModAsker(() => Promise.reject(new Error("daemon down")));
    await expect(askBeforeClaude()).resolves.toBeUndefined();
    uninstallFailing();
  });
});

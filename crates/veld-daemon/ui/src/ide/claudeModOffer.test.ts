import { describe, expect, it, vi } from "vitest";

import {
  CLAUDE_MOD_OFFER_ID,
  claudeLaunches,
  isClaudeLaunch,
  wantsClaudeModOffer,
} from "./claudeModOffer";

describe("the Claude mod offer", () => {
  it("is prompted by a Claude session starting, and nothing else", () => {
    expect(isClaudeLaunch("claude", "ready")).toBe(true);
    expect(isClaudeLaunch("claude", "working")).toBe(false);
    expect(isClaudeLaunch("codex", "ready")).toBe(false);
    expect(isClaudeLaunch(undefined, "ready")).toBe(false);
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

  it("tells its listeners about Claude launches only", () => {
    const heard = vi.fn();
    const stop = claudeLaunches.subscribe(heard);
    claudeLaunches.notice("claude", "working");
    claudeLaunches.notice("pi", "ready");
    claudeLaunches.notice("claude", "ready");
    stop();
    claudeLaunches.notice("claude", "ready");
    expect(heard).toHaveBeenCalledTimes(1);
  });
});

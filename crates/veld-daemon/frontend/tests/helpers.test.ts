import { describe, it, expect } from "vitest";
import {
  timeAgo,
  hasUnread,
  isCurrentPage,
  formatTrace,
  findThread,
  truncateMiddle,
} from "../src/feedback-overlay/helpers";
import type { Thread } from "../src/feedback-overlay/types";

function makeThread(overrides: Partial<Thread> = {}): Thread {
  return {
    id: "t1",
    scope: { type: "page", page_url: "/" },
    status: "open",
    messages: [],
    created_at: new Date().toISOString(),
    updated_at: new Date().toISOString(),
    ...overrides,
  };
}

describe("timeAgo", () => {
  it("returns seconds for recent times", () => {
    const now = new Date(Date.now() - 30_000).toISOString();
    expect(timeAgo(now)).toBe("30s ago");
  });

  it("returns minutes", () => {
    const then = new Date(Date.now() - 5 * 60_000).toISOString();
    expect(timeAgo(then)).toBe("5m ago");
  });

  it("returns hours", () => {
    const then = new Date(Date.now() - 3 * 3600_000).toISOString();
    expect(timeAgo(then)).toBe("3h ago");
  });

  it("returns days", () => {
    const then = new Date(Date.now() - 2 * 86400_000).toISOString();
    expect(timeAgo(then)).toBe("2d ago");
  });
});

describe("hasUnread", () => {
  it("returns false for thread with no messages", () => {
    const thread = makeThread();
    expect(hasUnread(thread)).toBe(false);
  });

  it("returns false for thread with only human messages", () => {
    const thread = makeThread({
      messages: [
        {
          id: "m1",
          body: "hello",
          author: "human" as const,
          created_at: new Date().toISOString(),
        },
      ],
    });
    expect(hasUnread(thread)).toBe(false);
  });

  it("returns true for thread with unseen agent message", () => {
    const thread = makeThread({
      messages: [
        {
          id: "m1",
          body: "reply",
          author: "agent" as const,
          created_at: new Date().toISOString(),
        },
      ],
    });
    expect(hasUnread(thread)).toBe(true);
  });

  it("returns false when the agent message has already been seen", () => {
    const thread = makeThread({
      messages: [
        {
          id: "m1",
          body: "reply",
          author: "agent" as const,
          created_at: new Date().toISOString(),
        },
      ],
      last_human_seen_seq: 1,
    });
    expect(hasUnread(thread)).toBe(false);
  });
});

// isCurrentPage depends on window.location — tested via e2e, not unit tests

describe("formatTrace", () => {
  it("returns null for empty trace", () => {
    expect(formatTrace(null)).toBeNull();
    expect(formatTrace([])).toBeNull();
  });

  it("joins short trace with >", () => {
    expect(formatTrace(["App", "Layout", "Page"])).toBe(
      "App > Layout > Page",
    );
  });

  it("deduplicates consecutive names", () => {
    expect(formatTrace(["App", "App", "Page"])).toBe("App > Page");
  });

  it("truncates long traces to last 5", () => {
    const long = ["A", "B", "C", "D", "E", "F", "G"];
    const result = formatTrace(long)!;
    expect(result).toBe("C > D > E > F > G");
  });
});

describe("findThread", () => {
  it("finds thread by id", () => {
    const threads = [makeThread({ id: "a" }), makeThread({ id: "b" })];
    expect(findThread(threads, "b")?.id).toBe("b");
  });

  it("returns undefined for missing id", () => {
    expect(findThread([], "x")).toBeUndefined();
  });
});

describe("truncateMiddle", () => {
  it("returns short text unchanged", () => {
    expect(truncateMiddle("hello world")).toBe("hello world");
  });

  it("collapses internal whitespace/newlines even when under the limit", () => {
    expect(truncateMiddle("hello   \n\n  world")).toBe("hello world");
  });

  it("truncates the middle, keeping a long head and short tail", () => {
    const text = "A".repeat(50) + "MIDDLE" + "B".repeat(50) + "END";
    const result = truncateMiddle(text, 30, 10);
    expect(result.length).toBe(30);
    expect(result.startsWith("A".repeat(19))).toBe(true);
    expect(result.endsWith("BBEND")).toBe(true); // true tail, not sliced off a head-bounded copy
    expect(result).toContain("…");
  });

  it("preserves the text's true end even when it's far beyond maxLen*10", () => {
    const text = "x".repeat(5000) + "THE-VERY-END";
    const result = truncateMiddle(text, 50, 12);
    expect(result.endsWith("THE-VERY-END")).toBe(true);
  });
});


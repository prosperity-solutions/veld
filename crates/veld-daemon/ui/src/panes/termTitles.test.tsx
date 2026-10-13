import { act, render, renderHook } from "@testing-library/react";
import { useRef } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { type PaneLayout, paneTabLabel } from "./model";
import {
  clearTermTitle,
  getTermTitle,
  resetTermTitlesForTest,
  setTermTitle,
  subscribeTermTitle,
  useTermTitle,
} from "./termTitles";

afterEach(resetTermTitlesForTest);

describe("term title store", () => {
  it("cleans a title before storing it", () => {
    setTermTitle("a", "build\u0007 step‮");
    expect(getTermTitle("a")).toBe("build  step");
  });

  it("clears on an empty or all-invisible title", () => {
    setTermTitle("a", "working");
    setTermTitle("a", "   ");
    expect(getTermTitle("a")).toBeUndefined();
    setTermTitle("a", "working");
    setTermTitle("a", "​﻿");
    expect(getTermTitle("a")).toBeUndefined();
  });

  it("notifies only the session that changed, and only on a real change", () => {
    const a = vi.fn();
    const b = vi.fn();
    subscribeTermTitle("a", a);
    subscribeTermTitle("b", b);
    setTermTitle("a", "one");
    setTermTitle("a", "one");
    expect(a).toHaveBeenCalledTimes(1);
    expect(b).not.toHaveBeenCalled();
    setTermTitle("a", "two");
    expect(a).toHaveBeenCalledTimes(2);
  });

  it("does not collapse spinner frames: a glyph change is a change", () => {
    // Claude Code's working -> idle transition is exactly a glyph swap.
    const a = vi.fn();
    subscribeTermTitle("a", a);
    setTermTitle("a", "⠋ fix login");
    setTermTitle("a", "⠙ fix login");
    setTermTitle("a", "✳ fix login");
    expect(a).toHaveBeenCalledTimes(3);
    expect(getTermTitle("a")).toBe("✳ fix login");
  });

  it("drops the entry when the session goes, and tells subscribers", () => {
    const a = vi.fn();
    setTermTitle("a", "one");
    subscribeTermTitle("a", a);
    clearTermTitle("a");
    expect(getTermTitle("a")).toBeUndefined();
    expect(a).toHaveBeenCalledTimes(1);
    // Clearing nothing is not news.
    clearTermTitle("a");
    expect(a).toHaveBeenCalledTimes(1);
  });

  it("unsubscribes", () => {
    const a = vi.fn();
    const off = subscribeTermTitle("a", a);
    off();
    setTermTitle("a", "one");
    expect(a).not.toHaveBeenCalled();
  });
});

describe("useTermTitle", () => {
  it("follows the store for its own session", () => {
    const { result } = renderHook(() => useTermTitle("a"));
    expect(result.current).toBeUndefined();
    act(() => setTermTitle("a", "hello"));
    expect(result.current).toBe("hello");
    act(() => clearTermTitle("a"));
    expect(result.current).toBeUndefined();
  });

  it("starts from the current value", () => {
    setTermTitle("a", "already here");
    const { result } = renderHook(() => useTermTitle("a"));
    expect(result.current).toBe("already here");
  });

  it("does not re-render for another session's title, or for no session", () => {
    let renders = 0;
    renderHook(() => {
      renders += 1;
      useTermTitle("a");
      useTermTitle(null);
    });
    const before = renders;
    act(() => {
      setTermTitle("b", "noise");
      setTermTitle("b", "more noise");
    });
    expect(renders).toBe(before);
  });

  it("re-renders only the component showing the changed session", () => {
    const counts = { a: 0, b: 0, parent: 0 };
    function Tab(props: { id: "a" | "b" }) {
      const t = useTermTitle(props.id);
      counts[props.id] += 1;
      return <span>{t}</span>;
    }
    function Parent() {
      // The parent is the stand-in for the app: it must not be subscribed.
      const n = useRef(0);
      n.current += 1;
      counts.parent = n.current;
      return (
        <>
          <Tab id="a" />
          <Tab id="b" />
        </>
      );
    }
    const { container } = render(<Parent />);
    const parent = counts.parent;
    const b = counts.b;
    act(() => setTermTitle("a", "⠋ one"));
    act(() => setTermTitle("a", "⠙ one"));
    expect(container.textContent).toBe("⠙ one");
    expect(counts.parent).toBe(parent);
    expect(counts.b).toBe(b);
  });

  it("switches subscription when the session id changes", () => {
    setTermTitle("a", "title a");
    setTermTitle("b", "title b");
    const { result, rerender } = renderHook(({ id }) => useTermTitle(id), {
      initialProps: { id: "a" as string | null },
    });
    expect(result.current).toBe("title a");
    rerender({ id: "b" });
    expect(result.current).toBe("title b");
    act(() => setTermTitle("a", "ignored"));
    expect(result.current).toBe("title b");
    rerender({ id: null });
    expect(result.current).toBeUndefined();
  });
});

describe("tab label from the store", () => {
  const layout = { docks: [{ tabs: [], activeId: null }, { tabs: [], activeId: null }] } as unknown as PaneLayout;
  const tab = { id: "t1", kind: "terminal", title: "Claude", spec: "claude" } as const;

  it("prefers the stored title and falls back to the pane's own name", () => {
    expect(paneTabLabel(layout, tab, getTermTitle("t1"))).toBe("Claude");
    setTermTitle("t1", "✳ fix the login flow");
    expect(paneTabLabel(layout, tab, getTermTitle("t1"))).toBe("✳ fix the login flow");
    // A fresh relaunch clears it: back to the label.
    setTermTitle("t1", "");
    expect(paneTabLabel(layout, tab, getTermTitle("t1"))).toBe("Claude");
  });
});

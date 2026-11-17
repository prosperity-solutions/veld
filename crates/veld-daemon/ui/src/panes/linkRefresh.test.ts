// @vitest-environment jsdom
import type { Terminal } from "@xterm/xterm";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { LINK_REFRESH_MS, LinkRefreshAddon, isLinkRefreshProbe } from "./linkRefresh";

const ROWS = 10;
const ROW_H = 20;
const TOP = 100;

/** The parts of an xterm `Terminal` the addon touches, with a 10-row screen at y=100. */
function fakeTerminal() {
  const element = document.createElement("div");
  element.className = "xterm";
  const screen = document.createElement("div");
  screen.className = "xterm-screen";
  element.appendChild(screen);
  document.body.appendChild(element);
  screen.getBoundingClientRect = () =>
    ({ top: TOP, left: 0, width: 800, height: ROWS * ROW_H, right: 800, bottom: TOP + ROWS * ROW_H }) as DOMRect;
  const renderListeners = new Set<(e: { start: number; end: number }) => void>();
  const term = {
    element,
    rows: ROWS,
    onRender: (fn: (e: { start: number; end: number }) => void) => {
      renderListeners.add(fn);
      return { dispose: () => renderListeners.delete(fn) };
    },
  };
  const render = (start: number, end: number) => {
    for (const fn of renderListeners) fn({ start, end });
  };
  return { term: term as unknown as Terminal, element, screen, render, renderListeners };
}

const rowY = (row: number) => TOP + (row + 0.5) * ROW_H;

describe("LinkRefreshAddon", () => {
  let f: ReturnType<typeof fakeTerminal>;
  let addon: LinkRefreshAddon;
  let moves: { y: number; bubbles: boolean; probe: boolean }[];

  beforeEach(() => {
    vi.useFakeTimers();
    f = fakeTerminal();
    addon = new LinkRefreshAddon();
    addon.activate(f.term);
    moves = [];
    // Sees every move; `realMove` clears the one it dispatches, so what is left is
    // the addon's.
    f.screen.addEventListener("mousemove", (e) =>
      moves.push({ y: e.clientY, bubbles: e.bubbles, probe: isLinkRefreshProbe(e) }),
    );
  });

  afterEach(() => {
    addon.dispose();
    f.element.remove();
    vi.useRealTimers();
  });

  const enter = (row: number) => {
    f.screen.dispatchEvent(new MouseEvent("mouseenter", { clientX: 40, clientY: rowY(row) }));
  };
  const realMove = (row: number, buttons = 0) => {
    f.screen.dispatchEvent(new MouseEvent("mousemove", { clientX: 40, clientY: rowY(row), buttons }));
    moves.length = 0;
  };

  it("moves to a neighbouring row and back when the pointer enters", () => {
    enter(3);
    expect(moves).toEqual([
      { y: rowY(2), bubbles: false, probe: true },
      { y: rowY(3), bubbles: false, probe: false },
    ]);
  });

  it("goes below instead of above on the top row", () => {
    enter(0);
    expect(moves.map((m) => m.y)).toEqual([rowY(1), rowY(0)]);
  });

  it("refreshes once after the row under the pointer is redrawn", () => {
    enter(4);
    realMove(4);
    f.render(4, 4);
    f.render(0, 9);
    expect(moves).toEqual([]);
    vi.advanceTimersByTime(LINK_REFRESH_MS);
    expect(moves.map((m) => m.y)).toEqual([rowY(3), rowY(4)]);
  });

  it("keeps refreshing while output streams, instead of waiting for it to stop", () => {
    enter(4);
    realMove(4);
    for (let i = 0; i < 6; i++) {
      f.render(4, 4);
      vi.advanceTimersByTime(LINK_REFRESH_MS / 2);
    }
    // 150 ms of redraws every 25 ms: one refresh per 50 ms, two moves each.
    expect(moves.length / 2).toBe(3);
  });

  it("ignores redraws of other rows", () => {
    enter(4);
    realMove(4);
    f.render(6, 9);
    vi.advanceTimersByTime(LINK_REFRESH_MS);
    expect(moves).toEqual([]);
  });

  it("does nothing while the pointer is outside the terminal", () => {
    enter(4);
    realMove(4);
    f.screen.dispatchEvent(new MouseEvent("mouseleave"));
    f.render(0, 9);
    vi.advanceTimersByTime(LINK_REFRESH_MS);
    expect(moves).toEqual([]);
  });

  it("does nothing while a button is held, so a drag-select is left alone", () => {
    enter(4);
    realMove(4, 1);
    f.render(4, 4);
    vi.advanceTimersByTime(LINK_REFRESH_MS);
    expect(moves).toEqual([]);
  });

  it("follows the pointer to the row it is on now", () => {
    enter(4);
    realMove(7);
    f.render(7, 7);
    vi.advanceTimersByTime(LINK_REFRESH_MS);
    expect(moves.map((m) => m.y)).toEqual([rowY(6), rowY(7)]);
  });

  it("stops listening when disposed", () => {
    enter(4);
    realMove(4);
    f.render(4, 4);
    addon.dispose();
    vi.advanceTimersByTime(LINK_REFRESH_MS);
    enter(4);
    expect(moves).toEqual([]);
    expect(f.renderListeners.size).toBe(0);
  });

  it("waits for the button to come up, since a refresh mid-click makes xterm drop the click", () => {
    enter(4);
    realMove(4);
    f.screen.dispatchEvent(new MouseEvent("mousedown", { buttons: 1 }));
    f.render(4, 4);
    vi.advanceTimersByTime(LINK_REFRESH_MS);
    expect(moves).toEqual([]);
    // The refresh that was due happens on release, without waiting for another redraw.
    f.screen.dispatchEvent(new MouseEvent("mouseup", { buttons: 0, bubbles: true }));
    expect(moves.map((m) => m.y)).toEqual([rowY(3), rowY(4)]);
  });

  it("drops the owed refresh when the button comes up outside the terminal", () => {
    enter(4);
    realMove(4);
    f.screen.dispatchEvent(new MouseEvent("mousedown", { buttons: 1 }));
    f.render(4, 4);
    vi.advanceTimersByTime(LINK_REFRESH_MS);
    document.body.dispatchEvent(new MouseEvent("mouseup", { buttons: 0, bubbles: true }));
    expect(moves).toEqual([]);
  });

  it("works for a terminal mounted under the pointer, which gets a move but no mouseenter", () => {
    realMove(4);
    f.render(4, 4);
    vi.advanceTimersByTime(LINK_REFRESH_MS);
    expect(moves.map((m) => m.y)).toEqual([rowY(3), rowY(4)]);
  });

  it("does nothing for a pointer position outside the rows", () => {
    enter(4);
    f.screen.dispatchEvent(new MouseEvent("mousemove", { clientX: 40, clientY: TOP + ROWS * ROW_H + 5 }));
    moves.length = 0;
    f.render(0, 9);
    vi.advanceTimersByTime(LINK_REFRESH_MS);
    expect(moves).toEqual([]);
  });

  it("warns and does nothing when xterm has no .xterm-screen", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const element = document.createElement("div");
    const other = new LinkRefreshAddon();
    other.activate({ element, rows: ROWS, onRender: () => ({ dispose() {} }) } as unknown as Terminal);
    expect(warn).toHaveBeenCalledOnce();
    other.dispose();
    warn.mockRestore();
  });

  it("does nothing once the pointer is seen elsewhere, though the terminal never saw a mouseleave", () => {
    enter(4);
    realMove(4);
    // Unmounted and remounted: no mouseleave, but the pointer is seen elsewhere.
    document.body.dispatchEvent(new MouseEvent("mousemove", { bubbles: true, clientX: 5, clientY: 5 }));
    f.render(4, 4);
    vi.advanceTimersByTime(LINK_REFRESH_MS);
    expect(moves).toEqual([]);
  });
});

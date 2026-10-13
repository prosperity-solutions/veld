import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ClaimEntry } from "./channel";

/** Tickets the daemon would mint, in order. */
let tickets: { ticket: string; expires_in_ms: number; client_id: string }[] = [];
let ticketError = false;

vi.mock("../api", () => ({
  api: {
    ideTicket: () => {
      if (ticketError) return Promise.reject(new Error("daemon down"));
      return Promise.resolve(tickets.shift() ?? { ticket: "t", expires_in_ms: 1000, client_id: "c" });
    },
  },
}));

/**
 * A `WebSocket` a test can drive.
 *
 * The suite runs in node with no DOM, so this is the whole transport. It records
 * what the client sent and lets a test push frames back.
 */
class FakeSocket {
  static instances: FakeSocket[] = [];
  static OPEN = 1;
  static CLOSED = 3;
  readyState = 0;
  sent: string[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: unknown }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;

  constructor(readonly url: string) {
    FakeSocket.instances.push(this);
  }

  send(data: string) {
    this.sent.push(data);
  }

  /** The handshake: open, then the daemon's `ready`. */
  open(clientId = "c", epoch = "e1") {
    this.readyState = FakeSocket.OPEN;
    this.onopen?.();
    this.push({ type: "ready", epoch, client_id: clientId });
  }

  push(msg: unknown) {
    this.onmessage?.({ data: JSON.stringify(msg) });
  }

  drop() {
    this.readyState = FakeSocket.CLOSED;
    this.onclose?.();
  }

  /** Everything the client sent, parsed. */
  frames(): Record<string, unknown>[] {
    return this.sent.map((s) => JSON.parse(s) as Record<string, unknown>);
  }
}

const storage = new Map<string, string>();

function handlers(over: Partial<Record<string, unknown>> = {}) {
  return {
    onClaims: () => {},
    onYield: () => {},
    onFocus: () => {},
    onLayoutChanged: () => {},
    onReady: () => {},
    onClosed: () => {},
    ...over,
  } as never;
}

/** A fresh module instance — `channel` is a singleton by design. */
async function freshChannel() {
  vi.resetModules();
  return (await import("./channel")).channel;
}

beforeEach(() => {
  vi.useFakeTimers();
  FakeSocket.instances = [];
  tickets = [];
  ticketError = false;
  storage.clear();
  vi.stubGlobal("WebSocket", FakeSocket);
  vi.stubGlobal("sessionStorage", {
    getItem: (k: string) => storage.get(k) ?? null,
    setItem: (k: string, v: string) => void storage.set(k, v),
  });
  vi.stubGlobal("window", { location: { href: "http://127.0.0.1:19899/ide" } });
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

/** Let the ticket promise settle so the socket exists. */
async function connect(channel: { start: (k: never, l: string, h: never) => void }) {
  channel.start("browser" as never, "Safari", handlers());
  await vi.advanceTimersByTimeAsync(0);
  return FakeSocket.instances.at(-1) as FakeSocket;
}

describe("the handshake", () => {
  it("sends a hello before anything else", async () => {
    const channel = await freshChannel();
    const ws = await connect(channel);
    ws.open();
    expect(ws.frames()[0]).toMatchObject({ type: "hello", kind: "browser", label: "Safari" });
  });

  /**
   * The identity is the daemon's — a client-chosen one let any client present
   * another's and inherit its claim with no yield asked.
   */
  it("does not send an identity it was not given", async () => {
    const channel = await freshChannel();
    const ws = await connect(channel);
    ws.open("minted-1");
    expect(ws.frames()[0]).not.toHaveProperty("client_id");
    expect(ws.frames()[0]).not.toHaveProperty("resume");
    expect(channel.identity).toBe("minted-1");
  });

  /** A reload asks for the identity it was given, so it gets its claims back. */
  it("asks to resume the identity a previous connection was given", async () => {
    storage.set("veld.clientId.v1", "previous-id");
    tickets = [{ ticket: "t", expires_in_ms: 1000, client_id: "fresh-id" }];
    const channel = await freshChannel();
    const ws = await connect(channel);
    ws.open("previous-id");
    expect(ws.frames()[0]).toMatchObject({ resume: "previous-id" });
  });

  it("ignores a stored identity that is not a valid one", async () => {
    storage.set("veld.clientId.v1", "not a valid id");
    const channel = await freshChannel();
    const ws = await connect(channel);
    ws.open();
    expect(ws.frames()[0]).not.toHaveProperty("resume");
  });
});

describe("claiming", () => {
  it("resolves with the daemon's answer", async () => {
    const channel = await freshChannel();
    const ws = await connect(channel);
    ws.open();
    const pending = channel.claim(7);
    await vi.advanceTimersByTimeAsync(0);
    const claim = ws.frames().find((f) => f.type === "claim");
    expect(claim).toMatchObject({ worktree_id: 7, focus_holder: true });
    ws.push({
      type: "claim_result",
      request_id: claim?.request_id,
      ok: false,
      reason: "shown_elsewhere",
      holder: { kind: "electron", label: "Window 1" },
    });
    await expect(pending).resolves.toMatchObject({
      ok: false,
      reason: "shown_elsewhere",
      holder: { kind: "electron" },
    });
  });

  /**
   * **A claim with no socket must not be granted.** HTTP and the socket fail
   * independently, so a client whose channel is down still reads a worktree's
   * real layout over `fetch` and attaches to its live shells — the takeover the
   * whole arbitration exists to prevent, with nothing arbitrating.
   */
  it("refuses rather than granting when the socket never comes up", async () => {
    ticketError = true;
    const channel = await freshChannel();
    channel.start("browser" as never, "Safari", handlers());
    const pending = channel.claim(7);
    await vi.advanceTimersByTimeAsync(10_000);
    await expect(pending).resolves.toMatchObject({ ok: false, reason: "offline" });
  });

  /** …but a click during the boot handshake must not be dropped either. */
  it("waits for a handshake that is still in flight", async () => {
    const channel = await freshChannel();
    const ws = await connect(channel);
    const pending = channel.claim(7);
    await vi.advanceTimersByTimeAsync(0);
    expect(ws.frames().some((f) => f.type === "claim")).toBe(false);
    ws.open();
    await vi.advanceTimersByTimeAsync(0);
    const claim = ws.frames().find((f) => f.type === "claim");
    expect(claim).toBeDefined();
    ws.push({ type: "claim_result", request_id: claim?.request_id, ok: true });
    await expect(pending).resolves.toMatchObject({ ok: true });
  });

  /** A dropped socket must answer its outstanding claims, and answer "no". */
  it("settles outstanding claims as refused when the socket drops", async () => {
    const channel = await freshChannel();
    const ws = await connect(channel);
    ws.open();
    const pending = channel.claim(7);
    await vi.advanceTimersByTimeAsync(0);
    ws.drop();
    await expect(pending).resolves.toMatchObject({ ok: false, reason: "disconnected" });
  });
});

describe("reconnecting", () => {
  /**
   * The daemon's copy of what this client holds died with the old socket. Until
   * it is back, a claim from another client is answered with no yield asked —
   * and that client attaches to shells this page is still driving.
   */
  it("re-reports what it holds", async () => {
    const channel = await freshChannel();
    const first = await connect(channel);
    first.open();
    channel.holds([7, 9]);
    first.drop();
    await vi.advanceTimersByTimeAsync(1000);
    const second = FakeSocket.instances.at(-1) as FakeSocket;
    expect(second).not.toBe(first);
    second.open();
    expect(second.frames()).toContainEqual({ type: "holds", worktree_ids: [7, 9] });
  });

  /**
   * And re-declares the shells, which has more riding on it than the holds do:
   * while the daemon has no set from this client, its reaper counts these panes
   * as belonging to nobody. A daemon restart is exactly when every terminal is
   * detached at once, so it is the worst moment to stay quiet about them.
   */
  it("re-declares the sessions it keeps alive", async () => {
    const channel = await freshChannel();
    const first = await connect(channel);
    first.open();
    channel.keep(["a", "b"]);
    expect(first.frames()).toContainEqual({ type: "keep", session_ids: ["a", "b"] });
    first.drop();
    await vi.advanceTimersByTimeAsync(1000);
    const second = FakeSocket.instances.at(-1) as FakeSocket;
    expect(second).not.toBe(first);
    second.open();
    expect(second.frames()).toContainEqual({ type: "keep", session_ids: ["a", "b"] });
  });

  /**
   * An empty set is a real answer — the last pane closed — but it is also the
   * state a page is in before it has read any layout. Sending it on every
   * reconnect would be a client with no terminals telling the daemon so, which
   * is true and costs nothing; not sending it is what keeps the re-declaration
   * to the frames that carry information.
   */
  it("says nothing about sessions it has none of", async () => {
    const channel = await freshChannel();
    const first = await connect(channel);
    first.open();
    first.drop();
    await vi.advanceTimersByTimeAsync(1000);
    const second = FakeSocket.instances.at(-1) as FakeSocket;
    second.open();
    expect(second.frames().some((f) => f.type === "keep")).toBe(false);
  });

  it("reports a daemon restart as a changed epoch, and a first connect as unchanged", async () => {
    const seen: boolean[] = [];
    vi.resetModules();
    const channel = (await import("./channel")).channel;
    channel.start("browser" as never, "S", handlers({ onReady: (same: boolean) => seen.push(same) }));
    await vi.advanceTimersByTimeAsync(0);
    const first = FakeSocket.instances.at(-1) as FakeSocket;
    first.open("c", "epoch-1");
    first.drop();
    await vi.advanceTimersByTimeAsync(1000);
    const second = FakeSocket.instances.at(-1) as FakeSocket;
    second.open("c", "epoch-2");
    expect(seen).toEqual([true, false]);
  });
});

/**
 * `holds` and `keep` are state replacements the daemon keeps as sets, and the
 * app calls both from effects that run on every layout change. A call that
 * changes nothing must not cost a frame.
 */
describe("repeating a set the daemon already has", () => {
  const ofType = (s: FakeSocket, type: string) => s.frames().filter((f) => f.type === type);

  it("sends keep once for an unchanged set, in any order", async () => {
    const channel = await freshChannel();
    const socket = await connect(channel);
    socket.open();
    channel.keep(["a", "b"]);
    channel.keep(["a", "b"]);
    channel.keep(["b", "a"]);
    channel.keep(["a", "b", "a"]);
    expect(ofType(socket, "keep")).toEqual([{ type: "keep", session_ids: ["a", "b"] }]);
    // A real change still goes out, and so does a return to the earlier set.
    channel.keep(["a"]);
    channel.keep(["a", "b"]);
    expect(ofType(socket, "keep")).toHaveLength(3);
    // Closing the last pane is a change too.
    channel.keep([]);
    expect(ofType(socket, "keep").at(-1)).toEqual({ type: "keep", session_ids: [] });
  });

  it("sends holds once for an unchanged set, in any order", async () => {
    const channel = await freshChannel();
    const socket = await connect(channel);
    socket.open();
    channel.holds([7, 9]);
    channel.holds([9, 7]);
    channel.holds([7, 9]);
    expect(ofType(socket, "holds")).toHaveLength(1);
    channel.holds([7]);
    expect(ofType(socket, "holds")).toHaveLength(2);
  });

  it("treats an empty set after the handshake as already known", async () => {
    // A fresh daemon-side client starts empty,
    // so an empty call after connect is a repeat, and a non-empty one is not.
    const channel = await freshChannel();
    const socket = await connect(channel);
    socket.open();
    channel.keep([]);
    channel.holds([]);
    expect(ofType(socket, "keep")).toHaveLength(0);
    expect(ofType(socket, "holds")).toHaveLength(0);
    channel.keep(["a"]);
    expect(ofType(socket, "keep")).toHaveLength(1);
  });

  it("does not record a frame it could not send", async () => {
    const channel = await freshChannel();
    const socket = await connect(channel);
    // Not open yet: the call is remembered for the handshake, not sent.
    channel.keep(["a"]);
    channel.holds([7]);
    expect(socket.frames()).toEqual([]);
    socket.open();
    // The handshake resent them once; asking again is a repeat.
    expect(ofType(socket, "keep")).toHaveLength(1);
    expect(ofType(socket, "holds")).toHaveLength(1);
    channel.keep(["a"]);
    channel.holds([7]);
    expect(ofType(socket, "keep")).toHaveLength(1);
    expect(ofType(socket, "holds")).toHaveLength(1);
  });

  it("still re-sends everything after a reconnect, then dedupes again", async () => {
    const channel = await freshChannel();
    const first = await connect(channel);
    first.open();
    channel.keep(["a", "b"]);
    channel.holds([7]);
    first.drop();
    // While the socket is down a repeat is swallowed (the handshake covers it)…
    channel.keep(["a", "b"]);
    await vi.advanceTimersByTimeAsync(1000);
    const second = FakeSocket.instances.at(-1) as FakeSocket;
    second.open();
    expect(ofType(second, "keep")).toEqual([{ type: "keep", session_ids: ["a", "b"] }]);
    expect(ofType(second, "holds")).toEqual([{ type: "holds", worktree_ids: [7] }]);
    // …and the new socket's copy is known, so the next repeat is quiet.
    channel.keep(["b", "a"]);
    channel.holds([7]);
    expect(ofType(second, "keep")).toHaveLength(1);
    expect(ofType(second, "holds")).toHaveLength(1);
  });

  it("sends a set that changed while the socket was down, once, at the handshake", async () => {
    const channel = await freshChannel();
    const first = await connect(channel);
    first.open();
    channel.keep(["a"]);
    first.drop();
    channel.keep(["a", "b"]);
    await vi.advanceTimersByTimeAsync(1000);
    const second = FakeSocket.instances.at(-1) as FakeSocket;
    second.open();
    expect(ofType(second, "keep")).toEqual([{ type: "keep", session_ids: ["a", "b"] }]);
  });

  it("re-sends holds after a forget, which the daemon may not have followed", async () => {
    const channel = await freshChannel();
    const socket = await connect(channel);
    socket.open();
    channel.holds([7, 9]);
    channel.forget([9]);
    // The daemon drops only ids it verifies are gone; this one may still be held
    // there, so the identical set must be said again rather than assumed.
    channel.holds([7]);
    expect(ofType(socket, "holds")).toEqual([
      { type: "holds", worktree_ids: [7, 9] },
      { type: "holds", worktree_ids: [7] },
    ]);
    // After which it is known again, and a repeat is quiet.
    channel.holds([7]);
    expect(ofType(socket, "holds")).toHaveLength(2);
    // And a reconnect resends what forget left, not the forgotten id.
    socket.drop();
    await vi.advanceTimersByTimeAsync(1000);
    const next = FakeSocket.instances.at(-1) as FakeSocket;
    next.open();
    expect(ofType(next, "holds")).toEqual([{ type: "holds", worktree_ids: [7] }]);
  });
});

describe("frames from the daemon", () => {
  it("routes a yield with an acknowledgement bound to its id", async () => {
    const acks: number[] = [];
    vi.resetModules();
    const channel = (await import("./channel")).channel;
    channel.start(
      "browser" as never,
      "S",
      handlers({
        onYield: (_id: number, ack: () => void) => {
          acks.push(_id);
          ack();
        },
      }),
    );
    await vi.advanceTimersByTimeAsync(0);
    const ws = FakeSocket.instances.at(-1) as FakeSocket;
    ws.open();
    ws.push({ type: "yield", worktree_id: 7, yield_id: 42 });
    expect(acks).toEqual([7]);
    expect(ws.frames()).toContainEqual({ type: "yielded", yield_id: 42 });
  });

  it("hands the claims table through as the daemon sees it", async () => {
    const tables: ClaimEntry[][] = [];
    vi.resetModules();
    const channel = (await import("./channel")).channel;
    channel.start(
      "browser" as never,
      "S",
      handlers({ onClaims: (c: ClaimEntry[]) => tables.push(c) }),
    );
    await vi.advanceTimersByTimeAsync(0);
    const ws = FakeSocket.instances.at(-1) as FakeSocket;
    ws.open();
    ws.push({
      type: "claims",
      claims: [{ worktree_id: 7, mine: true, client: { kind: "browser", label: "S" } }],
    });
    expect(tables.at(-1)).toEqual([
      { worktree_id: 7, mine: true, client: { kind: "browser", label: "S" } },
    ]);
  });

  /**
   * An older client meeting a newer daemon. Ignoring is right — every message
   * here is an optimisation over re-reading, never the only way to learn
   * something — but it must not throw.
   */
  it("ignores a message kind it does not know", async () => {
    const channel = await freshChannel();
    const ws = await connect(channel);
    ws.open();
    expect(() => ws.push({ type: "from-the-future", data: 1 })).not.toThrow();
    expect(() => ws.onmessage?.({ data: "not json" })).not.toThrow();
  });
});

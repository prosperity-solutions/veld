/**
 * The IDE control socket: this page's half of the daemon's claim registry.
 *
 * Replaces the Electron IPC channels that used to carry the same protocol
 * (`veld:window:claim`, `:claims`, `:yield`, `:yielded`, `:holds`). Those could
 * only ever see other *Electron windows*, so a plain browser tab was invisible
 * to them — it opened a worktree the desktop app already had, rendered its own
 * separate panes, and fought the app for every shell. The daemon is what both
 * kinds of client share, so the arbitration lives there now and this file is the
 * transport.
 *
 * **A WebSocket, not polling.** The daemon is `http://127.0.0.1:<port>`, i.e.
 * HTTP/1.1, where a browser allows about six concurrent connections *per origin
 * across every tab and window*. Veld Desktop opens up to eight windows against
 * that one origin, so anything that holds a request open — long-poll, SSE — would
 * spend the whole pool and stall the app's ordinary `fetch` calls behind it.
 * WebSockets are exempt from that limit. The yield handshake also needs a
 * genuine request/response with a deadline, which a poll cannot express: a poll
 * carries state, and a focus request is an event.
 *
 * **The socket is the lease.** The daemon releases everything this client holds
 * when this socket closes, so there is no heartbeat here and nothing to expire.
 * The one thing that must survive is the identity, which the daemon mints and
 * this page remembers in `sessionStorage` so a reload can ask for it back — see
 * `CLIENT_ID_KEY`.
 */

import { api } from "../api";
import { inbox, type AgentState } from "../inbox/inbox";

/** What kind of client this is, which decides whether it can be raised. */
export type ClientKind = "electron" | "browser";

/**
 * How the daemon describes a client to the others.
 *
 * No identity on it, deliberately: the id is the credential a reconnect resumes
 * with, so a broadcast carrying it would hand every client what it needs to
 * impersonate any other. A rail renders a kind and a label.
 */
export interface ClientInfo {
  kind: ClientKind;
  label: string;
}

/** One row of the claims table, as *this* client sees it. */
export interface ClaimEntry {
  worktree_id: number;
  /** Whether this client is the one showing it — answered per recipient by the
   *  daemon, which is what lets the identity stay off the wire. */
  mine: boolean;
  client: ClientInfo;
}

/** The outcome of asking to show a worktree. */
export interface ClaimResult {
  ok: boolean;
  /** `shown_elsewhere`, `superseded`, or `offline`. Absent on success. */
  reason?: string;
  /** Who has it, when the reason is `shown_elsewhere`. */
  holder?: ClientInfo;
  /** Identifies a *granted* claim, so it can be given back by name — see
   *  `release`. Absent on a refusal, which has nothing to give back. */
  seq?: number;
}

/**
 * Where this page remembers the identity the daemon gave it, so a reload can ask
 * for it back and get its claims with it.
 *
 * **The id is the daemon's, not ours.** It comes back with the ticket
 * (`api.ideTicket`), because a client-chosen one let any client present
 * another's and inherit its claim with no yield asked of the client still
 * attached to those terminals.
 *
 * `sessionStorage` rather than `localStorage`, because the latter is shared by
 * every tab in the profile — so two tabs would ask to resume one identity, and
 * the daemon refuses a resume while that id is connected, leaving the second tab
 * with the fresh id its own ticket carried. That is exactly right for Chrome's
 * "Duplicate Tab", which copies `sessionStorage`: the copy is a second client,
 * not a usurper.
 *
 * The cost is that a *restored* browser session (reopen the tab, "continue where
 * you left off") is a new client. That is correct: nothing of that tab's was
 * still attached, so it has nothing to reclaim.
 */
const CLIENT_ID_KEY = "veld.clientId.v1";

function rememberedId(): string | null {
  try {
    const existing = sessionStorage.getItem(CLIENT_ID_KEY);
    // Same charset the daemon validates. A value that fails it is dropped rather
    // than sent, so a hand-edited one costs the reclaim and nothing else.
    return existing && /^[A-Za-z0-9_-]{1,64}$/.test(existing) ? existing : null;
  } catch {
    // Storage access throws outright in some privacy configurations.
    return null;
  }
}

function remember(id: string): void {
  try {
    sessionStorage.setItem(CLIENT_ID_KEY, id);
  } catch {
    // A per-load identity still works; it just means a reload does not reclaim.
  }
}

/** How long to wait before reconnecting, backing off to `MAX_RETRY_MS`. */
const BASE_RETRY_MS = 300;
const MAX_RETRY_MS = 5000;

/**
 * How long a claim waits for the socket before answering "offline".
 *
 * Covers the boot case — the page renders before the handshake lands, and a
 * click in that window must not be dropped — without turning a genuinely absent
 * daemon into a hung rail.
 */
const CONNECT_WAIT_MS = 4000;

interface Handlers {
  /** The full claims table. State, not a delta — a client that missed one is
   *  not wrong afterwards. */
  onClaims(claims: ClaimEntry[]): void;
  /** Let go of this worktree's panes and call `ack` once the release is on
   *  screen. The client that asked does not attach until then. */
  onYield(worktreeId: number, ack: () => void): void;
  /** Somebody asked to be brought here. */
  onFocus(): void;
  /** A worktree's stored layout moved; re-read it. */
  onLayoutChanged(worktreeId: number, version: number): void;
  /**
   * The socket came up. `sameEpoch` is false when the daemon restarted since
   * the last connection — `veld update` replaces the binary while pages stay
   * open — which means every claim this client held is gone and anything it
   * cached about the daemon's state has to be re-read rather than trusted.
   */
  onReady(sameEpoch: boolean): void;
  /** The socket went away. Claims are not this client's any more. */
  onClosed(): void;
}

/**
 * The control socket.
 *
 * One instance per page, created at boot and never torn down: it is this page's
 * membership of the registry, so a lifecycle tied to a React effect would drop
 * every claim on a hot reload.
 */
class Channel {
  private ws: WebSocket | null = null;
  private handlers: Handlers | null = null;
  private retry = BASE_RETRY_MS;
  private epoch: string | null = null;
  private nextRequest = 1;
  /** Claims awaiting an answer, by `request_id`. */
  private pending = new Map<number, (r: ClaimResult) => void>();
  /** What this client holds, resent after every reconnect — the daemon's copy
   *  went with the old socket. */
  private held: number[] = [];
  private kind: ClientKind = "browser";
  private label = "";
  private started = false;
  private closed = false;
  /** The identity in use, once the daemon has told us. */
  private id: string | null = null;
  /** Whether the handshake has completed on the current socket. */
  private ready = false;
  /** Callers parked until it has. */
  private waiting: (() => void)[] = [];

  /** Whether the socket is up and the handshake done. */
  get connected(): boolean {
    return this.ready && this.ws?.readyState === WebSocket.OPEN;
  }

  /** The identity the daemon gave this connection, or `null` before the
   *  handshake. Used to tag layout writes so the daemon does not echo them
   *  back to their own author. */
  get identity(): string | null {
    return this.id;
  }

  start(kind: ClientKind, label: string, handlers: Handlers): void {
    this.kind = kind;
    this.label = label;
    this.handlers = handlers;
    if (this.started) return;
    this.started = true;
    void this.connect();
  }

  private url(ticket: string): string {
    const u = new URL("/api/ide/channel", window.location.href);
    u.protocol = u.protocol === "https:" ? "wss:" : "ws:";
    u.searchParams.set("ticket", ticket);
    return u.toString();
  }

  private async connect(): Promise<void> {
    if (this.closed) return;
    let ticket: string;
    let minted: string;
    try {
      const t = await api.ideTicket();
      ticket = t.ticket;
      minted = t.client_id;
    } catch {
      // The daemon is down or restarting. Nothing to report to the user here —
      // every other request in the app is failing too and says so.
      this.scheduleRetry();
      return;
    }
    if (this.closed) return;

    let ws: WebSocket;
    try {
      ws = new WebSocket(this.url(ticket));
    } catch {
      this.scheduleRetry();
      return;
    }
    this.ws = ws;

    ws.onopen = () => {
      // The hello must be the first frame: everything after it is scoped to the
      // identity it establishes. `resume` is a *request* — the daemon honours it
      // only if nothing is connected under that id, and tells us in `ready`
      // which identity we actually got.
      const resume = rememberedId();
      ws.send(
        JSON.stringify({
          type: "hello",
          ...(resume && resume !== minted ? { resume } : {}),
          kind: this.kind,
          label: this.label,
        }),
      );
      // Re-report what this page holds. The daemon's copy died with the old
      // socket, and until it is back the daemon would not know to ask this
      // client to yield — so another client claiming one of these worktrees
      // would attach to shells this page is still driving.
      if (this.held.length > 0) {
        ws.send(JSON.stringify({ type: "holds", worktree_ids: this.held }));
      }
    };

    ws.onmessage = (ev) => {
      if (typeof ev.data !== "string") return;
      let msg: Record<string, unknown>;
      try {
        msg = JSON.parse(ev.data) as Record<string, unknown>;
      } catch {
        return;
      }
      this.dispatch(msg);
    };

    const drop = () => {
      if (this.ws !== ws) return;
      this.ws = null;
      this.ready = false;
      // Every outstanding claim is unanswerable now. Resolving them as refused
      // rather than leaving them hanging is what keeps a caller from awaiting
      // forever — and "not ok" is the safe direction, because a caller that
      // treats a non-ok answer as "stay put" cannot attach to shells it was
      // never granted.
      const waiting = [...this.pending.values()];
      this.pending.clear();
      for (const settle of waiting) settle({ ok: false, reason: "disconnected" });
      this.handlers?.onClosed();
      this.scheduleRetry();
    };
    ws.onclose = drop;
    ws.onerror = drop;
  }

  private dispatch(msg: Record<string, unknown>): void {
    const h = this.handlers;
    if (!h) return;
    switch (msg.type) {
      case "ready": {
        const epoch = typeof msg.epoch === "string" ? msg.epoch : null;
        // First connection of this page counts as "same": there is nothing it
        // could have missed.
        const same = this.epoch === null || this.epoch === epoch;
        this.epoch = epoch;
        this.retry = BASE_RETRY_MS;
        if (typeof msg.client_id === "string") {
          this.id = msg.client_id;
          remember(msg.client_id);
        }
        // Anything queued while the socket was down goes now — including the
        // re-claim the app issues from here, which is what stops a daemon
        // restart leaving two clients each believing they own a worktree.
        this.ready = true;
        const waiting = this.waiting.splice(0);
        h.onReady(same);
        for (const resolve of waiting) resolve();
        return;
      }
      case "claims": {
        h.onClaims(Array.isArray(msg.claims) ? (msg.claims as ClaimEntry[]) : []);
        return;
      }
      case "claim_result": {
        const id = typeof msg.request_id === "number" ? msg.request_id : -1;
        const settle = this.pending.get(id);
        if (!settle) return;
        this.pending.delete(id);
        settle({
          ok: msg.ok === true,
          reason: typeof msg.reason === "string" ? msg.reason : undefined,
          holder: (msg.holder as ClientInfo | null) ?? undefined,
          seq: typeof msg.seq === "number" ? msg.seq : undefined,
        });
        return;
      }
      case "yield": {
        const worktreeId = msg.worktree_id;
        const yieldId = msg.yield_id;
        if (typeof worktreeId !== "number" || typeof yieldId !== "number") return;
        h.onYield(worktreeId, () => this.send({ type: "yielded", yield_id: yieldId }));
        return;
      }
      case "focus":
        h.onFocus();
        return;
      case "layout_changed": {
        if (typeof msg.worktree_id !== "number" || typeof msg.version !== "number") return;
        h.onLayoutChanged(msg.worktree_id, msg.version);
        return;
      }
      case "agent_state": {
        // Filed straight into the inbox rather than routed through `Handlers`, for the
        // same reason `panes/terminalHost.ts` files its own signals there: the inbox is
        // a store, not a policy the app has to arbitrate. The policy — read-on-focus,
        // read-on-type, mark-all-read — lives with the thing that knows where the user
        // is looking, and none of it is needed to *record* an event.
        //
        // Every field is checked because this arrives over a socket from a daemon that
        // may be a different version. An unrecognised `state` is dropped by the store,
        // so a newer daemon inventing one is silence rather than a wrong badge.
        if (
          typeof msg.worktree_id !== "number" ||
          typeof msg.session_id !== "string" ||
          typeof msg.state !== "string"
        ) {
          return;
        }
        inbox.report(msg.session_id, msg.worktree_id, {
          type: "agent",
          state: msg.state as AgentState,
          // A hook is the only producer of this message today, and the authority is the
          // daemon's claim about *how it learned*, not a field the wire carries — there
          // is nothing on the other end that could report at a lower one.
          source: "hook",
        });
        return;
      }
      default:
        // An older client meeting a newer daemon. Ignoring is right: every
        // message here is an optimisation over re-reading, never the only way
        // to learn something.
        return;
    }
  }

  private send(payload: Record<string, unknown>): void {
    if (this.ws?.readyState !== WebSocket.OPEN) return;
    this.ws.send(JSON.stringify(payload));
  }

  private scheduleRetry(): void {
    if (this.closed) return;
    const wait = this.retry;
    this.retry = Math.min(this.retry * 2, MAX_RETRY_MS);
    setTimeout(() => void this.connect(), wait);
  }

  /**
   * Ask to show a worktree.
   *
   * Resolves only once every other holder has let go, because the caller
   * attaches to the PTY sessions the layout names on the strength of this
   * answer — so it can take up to the daemon's acknowledgement timeout. Which
   * is also why answers do not arrive in call order.
   *
   * **With no socket this waits, and then refuses.** Answering `ok` was the
   * first version and it was wrong in the way that matters: HTTP and the socket
   * fail independently, so a page whose channel is down (the reconnect backoff
   * after a daemon restart, an origin the upgrade refuses) still reads the
   * worktree's real layout over `fetch` and attaches to its live shells — the
   * exact takeover the arbitration exists to prevent, with nothing arbitrating.
   * A short wait first, because at boot the socket is legitimately still
   * connecting and refusing there would make the first click do nothing.
   */
  async claim(worktreeId: number, focusHolder = true): Promise<ClaimResult> {
    if (!this.connected) {
      await this.whenReady();
      if (!this.connected) return { ok: false, reason: "offline" };
    }
    const requestId = this.nextRequest++;
    return new Promise<ClaimResult>((resolve) => {
      this.pending.set(requestId, resolve);
      this.send({
        type: "claim",
        worktree_id: worktreeId,
        request_id: requestId,
        focus_holder: focusHolder,
      });
    });
  }

  /**
   * Resolve when the handshake lands, or when [`CONNECT_WAIT_MS`] runs out.
   *
   * Bounded rather than open-ended: a click has to answer, and "the daemon is
   * not there" is a real answer. It resolves either way — the caller re-checks
   * `connected`, so a timeout is not an error to handle in two places.
   */
  private whenReady(): Promise<void> {
    if (this.ready) return Promise.resolve();
    return new Promise<void>((resolve) => {
      let settled = false;
      const once = () => {
        if (settled) return;
        settled = true;
        resolve();
      };
      this.waiting.push(once);
      setTimeout(once, CONNECT_WAIT_MS);
    });
  }

  /** Which worktrees this page has panes mounted for. */
  holds(worktreeIds: number[]): void {
    this.held = worktreeIds;
    this.send({ type: "holds", worktree_ids: worktreeIds });
  }

  /**
   * Give a worktree back without taking another.
   *
   * The daemon had no message for this, and a claim was otherwise undone only by
   * claiming something else or by disconnecting — so a client granted a worktree
   * it then decided not to show held it for the life of its socket, greyed out
   * in every other client's rail as shown by a window that is showing nothing.
   *
   * `seq` names the claim being given back. A release is handled inline while a
   * claim is spawned, so one sent *after* a claim can be processed before it —
   * and a cancelled acquire's late grant would otherwise erase the worktree a
   * newer acquire had just been granted.
   */
  release(worktreeId: number, seq: number): void {
    this.send({ type: "release", worktree_id: worktreeId, seq });
  }

  /** These worktrees are gone — rowids are reused, so a stale claim would grey
   *  out whichever one is created next. */
  forget(worktreeIds: number[]): void {
    if (worktreeIds.length === 0) return;
    this.held = this.held.filter((id) => !worktreeIds.includes(id));
    this.send({ type: "forget", worktree_ids: worktreeIds });
  }
}

export const channel = new Channel();

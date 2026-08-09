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
 * The one thing that must survive is the identity: `clientId` lives in
 * `sessionStorage`, so a reload reconnects as the same client and gets its
 * claims back, while a second tab is genuinely a second client.
 */

import { api } from "../api";

/** What kind of client this is, which decides whether it can be raised. */
export type ClientKind = "electron" | "browser";

/** How the daemon describes a client to the others. */
export interface ClientInfo {
  client_id: string;
  kind: ClientKind;
  label: string;
}

/** One row of the claims table. */
export interface ClaimEntry {
  worktree_id: number;
  client: ClientInfo;
}

/** The outcome of asking to show a worktree. */
export interface ClaimResult {
  ok: boolean;
  /** `shown_elsewhere` or `superseded`. Absent on success. */
  reason?: string;
  /** Who has it, when the reason is `shown_elsewhere`. */
  holder?: ClientInfo;
}

/**
 * This client's identity, stable across reloads of this tab or window.
 *
 * `sessionStorage`, deliberately, and this is the one decision in the file worth
 * arguing about. `localStorage` would be shared by every tab in the browser
 * profile, so two tabs would claim under one identity and the daemon would
 * consider them the same client — each would be sent the other's yields and each
 * would think the other had answered. A per-tab id is what makes "this tab holds
 * worktree 7" a true statement.
 *
 * The cost is that a *restored* browser session (reopen the tab, "continue where
 * you left off") is a new client. That is correct: nothing of that tab's was
 * still attached, so it has nothing to reclaim.
 */
const CLIENT_ID_KEY = "veld.clientId.v1";

function readClientId(): string {
  try {
    const existing = sessionStorage.getItem(CLIENT_ID_KEY);
    // Same charset the daemon validates, so a hand-edited value is replaced
    // rather than refused at the handshake — where the failure would be a page
    // that silently never claims anything.
    if (existing && /^[A-Za-z0-9_-]{1,64}$/.test(existing)) return existing;
    const fresh = crypto.randomUUID();
    sessionStorage.setItem(CLIENT_ID_KEY, fresh);
    return fresh;
  } catch {
    // Storage throws outright in some privacy configurations. A per-load id
    // still works; it just means a reload does not reclaim.
    return crypto.randomUUID();
  }
}

export const clientId: string = readClientId();

/** How long to wait before reconnecting, backing off to `MAX_RETRY_MS`. */
const BASE_RETRY_MS = 300;
const MAX_RETRY_MS = 5000;

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

  /** Whether the socket is up right now. */
  get connected(): boolean {
    return this.ws?.readyState === WebSocket.OPEN;
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
    try {
      ({ ticket } = await api.ideTicket());
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
      // identity it establishes.
      ws.send(
        JSON.stringify({
          type: "hello",
          client_id: clientId,
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
        h.onReady(same);
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
   * With no socket the answer is `ok` and nothing is recorded: a page that
   * cannot reach the daemon has no terminals to fight over either, and refusing
   * would leave the rail unusable for the whole of a daemon restart.
   */
  claim(worktreeId: number, focusHolder = true): Promise<ClaimResult> {
    if (this.ws?.readyState !== WebSocket.OPEN) {
      return Promise.resolve({ ok: true, reason: "offline" });
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

  /** Which worktrees this page has panes mounted for. */
  holds(worktreeIds: number[]): void {
    this.held = worktreeIds;
    this.send({ type: "holds", worktree_ids: worktreeIds });
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

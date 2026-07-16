// "Web sharing" card — start/stop a public web share for this page's run and
// show the live tunnel transport (direct vs relayed), all without leaving the
// browser. Drives the daemon's share control API same-origin under
// /__veld__/ (Caddy proxies it and injects X-Veld-Run, so the daemon knows
// which run the page belongs to even when several are active).
//
// Transport matters to surface here: a share riding a public relay is
// bandwidth-throttled — pages feel broken (stalled chunks, dead HMR) while
// every individual request "works". Naming the relay turns a mystery into a
// diagnosis.
import { refs } from "./refs";
import { mkEl } from "./helpers";
import { PREFIX } from "./constants";
import { toast } from "./toast";
import { toPublicLocation } from "./public-url";

export interface ShareConnectionInfo {
  node_id: string;
  label?: string;
  transport: "direct" | "relayed" | "none";
  via?: string;
  rtt_ms?: number;
}

interface GatewayPublicUrl {
  node: string;
  hostname: string;
  public_url: string;
  /** "password" | "link"; absent from pre-access-layer daemons. */
  access?: string;
}

export interface ShareInfo {
  id: string;
  public_urls?: GatewayPublicUrl[];
  web_password?: string;
  connections?: ShareConnectionInfo[];
}

interface SharesList {
  shares?: ShareInfo[];
}

/** How often the open card refreshes its share/transport state. Local-only
 * calls (Caddy → daemon on localhost), cheap; live enough to watch a tunnel
 * upgrade from relayed to direct after hole-punching lands. */
const REFRESH_MS = 3000;

let card: HTMLElement | null = null;
let refreshTimer: ReturnType<typeof setInterval> | null = null;
/** Guards the start/stop buttons against double-submit across refreshes. */
let busy = false;

/** Arc-menu entry point: toggle the card. */
export function toggleWebShareCard(): void {
  if (card) {
    closeWebShareCard();
  } else {
    openCard();
  }
}

export function closeWebShareCard(): void {
  if (refreshTimer) {
    clearInterval(refreshTimer);
    refreshTimer = null;
  }
  if (card) {
    card.remove();
    card = null;
  }
}

function openCard(): void {
  card = mkEl("div", "popover web-share-card");
  card.appendChild(mkEl("div", "popover-selector", "Web sharing"));
  const body = mkEl("div", "popover-body");
  body.appendChild(mkEl("div", "web-share-muted", "Loading…"));
  card.appendChild(body);
  refs.shadow.appendChild(card);
  void refresh();
  refreshTimer = setInterval(() => void refresh(), REFRESH_MS);
}

/** The active web share exposing this page's hostname, if any. */
export function findShare(list: SharesList, hostname: string): ShareInfo | null {
  for (const share of list.shares ?? []) {
    for (const u of share.public_urls ?? []) {
      if (u.hostname === hostname) return share;
    }
  }
  return null;
}

async function refresh(): Promise<void> {
  if (!card || busy) return;
  let list: SharesList;
  try {
    const r = await fetch("/__veld__/api/shares");
    if (!r.ok) throw new Error(String(r.status));
    list = (await r.json()) as SharesList;
  } catch (_) {
    renderError("Could not reach Veld");
    return;
  }
  render(findShare(list, window.location.hostname));
}

function body(): HTMLElement | null {
  return card?.querySelector("." + PREFIX + "popover-body") ?? null;
}

function renderError(msg: string): void {
  const b = body();
  if (!b) return;
  b.textContent = "";
  b.appendChild(mkEl("div", "web-share-muted", msg));
}

function render(share: ShareInfo | null): void {
  const b = body();
  if (!b) return;
  b.textContent = "";
  if (!share) {
    renderNotShared(b);
  } else {
    renderShared(b, share);
  }
}

// ── not shared ──────────────────────────────────────────────────────────────

function renderNotShared(b: HTMLElement): void {
  b.appendChild(
    mkEl(
      "div",
      "web-share-muted",
      "This page is not on the public web. Sharing mints a password-protected URL anyone can open in a browser.",
    ),
  );
  const actions = mkEl("div", "popover-actions");
  const shareBtn = mkEl(
    "button",
    "btn btn-primary btn-sm",
    "Share to web",
  ) as HTMLButtonElement;
  shareBtn.addEventListener("click", () => void startShare(shareBtn));
  actions.appendChild(shareBtn);
  b.appendChild(actions);
}

async function startShare(btn: HTMLButtonElement): Promise<void> {
  if (busy) return;
  busy = true;
  btn.disabled = true;
  btn.textContent = "Sharing…";
  try {
    // The run is resolved server-side from Caddy's X-Veld-Run header, so this
    // works with several runs active. X-Veld-Request is the localhost-CSRF
    // convention every mutating daemon route requires.
    const r = await fetch("/__veld__/api/shares", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "X-Veld-Request": "overlay",
      },
      body: JSON.stringify({ run: null, nodes: null, ttl_secs: null, web: true }),
    });
    if (!r.ok) {
      const detail = (await r.text()).trim();
      renderError(detail || "Sharing failed (" + r.status + ")");
      toast("Sharing failed", true);
      return;
    }
    toast("Shared to the web");
    await forceRefresh();
  } catch (_) {
    renderError("Could not reach Veld");
  } finally {
    busy = false;
  }
}

// ── shared ──────────────────────────────────────────────────────────────────

function renderShared(b: HTMLElement, share: ShareInfo): void {
  const target = (share.public_urls ?? []).find(
    (u) => u.hostname === window.location.hostname,
  );
  if (target) {
    const urlRow = mkEl("div", "web-share-url", target.public_url);
    urlRow.title = "Copy public link for this page";
    urlRow.addEventListener("click", () => {
      const password =
        target.access !== "link" && share.web_password
          ? share.web_password
          : null;
      const url = toPublicLocation(target.public_url, window.location, password);
      navigator.clipboard.writeText(url).then(
        () =>
          toast(
            password
              ? "Public one-link copied (carries the viewer password)"
              : "Public URL copied",
          ),
        () => toast("Copy failed — public URL: " + url),
      );
    });
    b.appendChild(urlRow);
    if (share.web_password) {
      b.appendChild(
        mkEl("div", "web-share-muted", "Password: " + share.web_password),
      );
    }
  }

  for (const c of share.connections ?? []) {
    b.appendChild(connectionRow(c));
  }
  if (!(share.connections ?? []).length) {
    b.appendChild(mkEl("div", "web-share-muted", "No viewer connected yet."));
  }

  const actions = mkEl("div", "popover-actions");
  const stopBtn = mkEl(
    "button",
    "btn btn-danger btn-sm",
    "Stop sharing",
  ) as HTMLButtonElement;
  stopBtn.addEventListener("click", () => void stopShare(share.id, stopBtn));
  actions.appendChild(stopBtn);
  b.appendChild(actions);
}

/** One tunnel line: who is connected, direct or relayed, and the cost. */
export function connectionRow(c: ShareConnectionInfo): HTMLElement {
  const row = mkEl("div", "web-share-conn");
  const who = c.label || c.node_id.slice(0, 10);
  const rtt = c.rtt_ms != null ? ", rtt " + c.rtt_ms + "ms" : "";
  if (c.transport === "direct") {
    row.appendChild(mkEl("span", "web-share-dot web-share-dot-direct"));
    // Same shape as the CLI's line: `direct (<addr>, rtt Xms)`.
    const detail = [c.via, c.rtt_ms != null ? "rtt " + c.rtt_ms + "ms" : null]
      .filter(Boolean)
      .join(", ");
    row.appendChild(
      mkEl(
        "span",
        "web-share-conn-text",
        who + ": direct" + (detail ? " (" + detail + ")" : ""),
      ),
    );
  } else if (c.transport === "relayed") {
    row.appendChild(mkEl("span", "web-share-dot web-share-dot-relayed"));
    row.appendChild(
      mkEl(
        "span",
        "web-share-conn-text",
        who +
          ": relayed via " +
          (c.via || "unknown relay") +
          rtt +
          " — throughput limited by the relay",
      ),
    );
  } else {
    row.appendChild(mkEl("span", "web-share-dot"));
    row.appendChild(mkEl("span", "web-share-conn-text", who + ": no open path"));
  }
  return row;
}

async function stopShare(id: string, btn: HTMLButtonElement): Promise<void> {
  if (busy) return;
  busy = true;
  btn.disabled = true;
  try {
    const r = await fetch("/__veld__/api/shares/" + encodeURIComponent(id), {
      method: "DELETE",
      headers: { "X-Veld-Request": "overlay" },
    });
    if (!r.ok) {
      toast("Stop failed (" + r.status + ")", true);
      return;
    }
    toast("Web share stopped");
    await forceRefresh();
  } catch (_) {
    toast("Could not reach Veld", true);
  } finally {
    busy = false;
  }
}

/** Refresh ignoring the busy latch (used right after a mutation completes,
 * while `busy` is still held to keep the interval refresh out). */
async function forceRefresh(): Promise<void> {
  if (!card) return;
  try {
    const r = await fetch("/__veld__/api/shares");
    if (r.ok) render(findShare((await r.json()) as SharesList, window.location.hostname));
  } catch (_) {
    /* next interval tick will retry */
  }
}

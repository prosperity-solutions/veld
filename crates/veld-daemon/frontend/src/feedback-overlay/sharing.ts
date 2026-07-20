// Sharing — public web share control, surfaced as a top-level toolbar submenu.
//
// This replaces the old "Web sharing" card: starting, stopping, copying the
// URL and reading the status are now radial menu actions, and a live status
// dot on the Sharing bubble shows at a glance whether THIS page is on the
// public web. Relay/transport detail (direct vs relayed, RTT, throughput
// warnings) is intentionally NOT shown here — that diagnosis belongs on the
// management UI; the in-page toolbar stays lean.
//
// All calls go same-origin under /__veld__/ (Caddy proxies them to the daemon
// and injects X-Veld-Run, so the daemon resolves which run the page belongs to
// even when several are active).
import { getState, dispatch } from "./store";
import { mkEl } from "./helpers";
import { PREFIX, ICONS } from "./constants";
import { toast } from "./toast";
import { makeToolBtn } from "./toolbar";
import type { ArcItem } from "./arc-menu";
import { copyPublicUrl, findPublicUrl } from "./public-url";

interface GatewayPublicUrl {
  node: string;
  hostname: string;
  public_url: string;
  /** "password" | "link"; absent from pre-access-layer daemons. */
  access?: string;
}

interface ShareInfo {
  id: string;
  public_urls?: GatewayPublicUrl[];
  web_password?: string;
}

interface SharesList {
  shares?: ShareInfo[];
}

/** Guards start/stop against double-submit across a poll tick. */
let busy = false;

/** The Sharing bubble's status dot — lit while this page is web-shared.
 *  Held here (not in refs) since only this module reads it. */
let statusDot: HTMLElement | null = null;

/** The active web share exposing `hostname`, if any. */
export function findShare(list: SharesList, hostname: string): ShareInfo | null {
  for (const share of list.shares ?? []) {
    for (const u of share.public_urls ?? []) {
      if (u.hostname === hostname) return share;
    }
  }
  return null;
}

/** Reflect the current share state on the Sharing bubble's status dot. */
export function updateSharingIndicator(): void {
  if (statusDot) {
    statusDot.classList.toggle(PREFIX + "status-dot-on", getState().shareActive);
  }
}

/**
 * Refresh the live share status for this page's hostname into the store.
 * Cheap (Caddy → daemon on localhost); called on a timer and right after a
 * start/stop so the dot and submenu reflect reality quickly.
 */
export function pollShareStatus(): void {
  fetch("/__veld__/api/shares")
    .then((r) => (r.ok ? (r.json() as Promise<SharesList>) : Promise.reject()))
    .then((list) => {
      const share = findShare(list, window.location.hostname);
      const active = !!share;
      const id = share?.id ?? null;
      if (active !== getState().shareActive || id !== getState().shareId) {
        dispatch({ type: "SET_SHARE_STATUS", active, id });
        updateSharingIndicator();
      }
    })
    .catch(() => {
      // Daemon briefly unreachable: leave the last known state; the next tick
      // (or a manual action) will reconcile.
    });
}

async function startSharing(): Promise<void> {
  if (busy) return;
  busy = true;
  try {
    // The run is resolved server-side from Caddy's X-Veld-Run header, so this
    // works with several runs active. X-Veld-Request is the localhost-CSRF
    // convention every mutating daemon route requires.
    const r = await fetch("/__veld__/api/shares", {
      method: "POST",
      headers: { "Content-Type": "application/json", "X-Veld-Request": "overlay" },
      body: JSON.stringify({ run: null, nodes: null, ttl_secs: null, web: true }),
    });
    if (!r.ok) {
      // The daemon's plain-text error names the fix (no gateway configured,
      // no web-opted nodes, …).
      const detail = (await r.text()).trim();
      toast(detail || "Sharing failed (" + r.status + ")", true);
      return;
    }
    const resp = (await r.json()) as { public_urls?: { hostname: string }[] };
    const covered = (resp.public_urls ?? []).some(
      (u) => u.hostname === window.location.hostname,
    );
    toast(
      covered
        ? "Shared to the web"
        : 'Shared to the web (other services) — add "web" to this service\'s share.expose in veld.json.',
    );
  } catch (_) {
    toast("Could not reach Veld", true);
  } finally {
    busy = false;
    pollShareStatus();
  }
}

async function stopSharing(): Promise<void> {
  const id = getState().shareId;
  if (!id || busy) return;
  busy = true;
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
  } catch (_) {
    toast("Could not reach Veld", true);
  } finally {
    busy = false;
    pollShareStatus();
  }
}

/** Toast a plain-language status readout (no relay/transport detail). */
async function showSharingStatus(): Promise<void> {
  let list: SharesList;
  try {
    const r = await fetch("/__veld__/api/shares");
    if (!r.ok) throw new Error(String(r.status));
    list = (await r.json()) as SharesList;
  } catch (_) {
    toast("Could not reach Veld", true);
    return;
  }
  const target = findPublicUrl(list, window.location.hostname);
  toast(
    target
      ? "Shared to the web: " + target.publicUrl
      : "Not shared to the web. Use Start sharing.",
  );
}

/**
 * Build the top-level "Sharing" menu item (with its submenu + status dot).
 * The submenu is data-driven: Start is shown when not shared, Stop + Copy when
 * shared, and Status always — the arc engine re-reads `isVisible` each time the
 * submenu is opened, so it always matches the current state.
 */
export function buildSharingMenuItem(): ArcItem {
  const shareBtn = makeToolBtn("sharing", ICONS.share);
  statusDot = mkEl("span", "status-dot");
  shareBtn.appendChild(statusDot);
  updateSharingIndicator();

  const sub: ArcItem[] = [
    {
      id: "share-start",
      el: makeToolBtn("share-start", ICONS.globe),
      label: "Start sharing",
      isVisible: () => !getState().shareActive,
      onSelect: () => { void startSharing(); },
    },
    {
      id: "share-stop",
      el: makeToolBtn("share-stop", ICONS.stop),
      label: "Stop sharing",
      isVisible: () => getState().shareActive,
      onSelect: () => { void stopSharing(); },
    },
    {
      id: "share-copy",
      el: makeToolBtn("share-copy", ICONS.copy),
      label: "Copy public URL",
      isVisible: () => getState().shareActive,
      onSelect: () => { void copyPublicUrl(); },
    },
    {
      id: "share-status",
      el: makeToolBtn("share-status", ICONS.info),
      label: "Sharing status",
      onSelect: () => { void showSharingStatus(); },
    },
  ];

  return {
    id: "sharing",
    el: shareBtn,
    label: "Sharing",
    sub,
  };
}

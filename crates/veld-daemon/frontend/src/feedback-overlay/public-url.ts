// "Copy public URL" — translate the current browser location to its public
// gateway URL: swap the host for the share's minted public host, keep
// path + query + hash so a deep link to the current screen survives the
// copy. For a password-protected share (the default) the
// copied link is the ONE-LINK form — the share password rides in a
// `veld-key` URL fragment, which never reaches DNS/TLS/server logs, and the
// gateway's login page auto-submits it — so the recipient gets a link that
// just opens.
//
// The daemon's share list is fetched on demand (no polling): the overlay is
// served same-origin under /__veld__/, which Caddy proxies to the daemon.
import { toast } from "./toast";

interface GatewayPublicUrl {
  node: string;
  hostname: string;
  public_url: string;
  /** "password" | "link"; absent from pre-access-layer daemons. */
  access?: string;
}

interface ShareInfo {
  public_urls?: GatewayPublicUrl[];
  /** The share password (password-protected web shares only). */
  web_password?: string;
}

interface SharesList {
  shares?: ShareInfo[];
}

/** A hostname's public target: minted URL + the password gating it (if any). */
export interface PublicTarget {
  publicUrl: string;
  /** Set when the slug is password-mode and the share carries a password. */
  password: string | null;
}

/** The public target for `hostname` among the daemon's active web shares. */
export function findPublicUrl(
  list: SharesList,
  hostname: string,
): PublicTarget | null {
  for (const share of list.shares ?? []) {
    for (const u of share.public_urls ?? []) {
      if (u.hostname === hostname) {
        const password =
          u.access !== "link" && share.web_password ? share.web_password : null;
        return { publicUrl: u.public_url, password };
      }
    }
  }
  return null;
}

/**
 * `public_url` + the location's path, query, and hash. When `password` is
 * set, a `veld-key` fragment is appended (joined with `&` if the page already
 * has a hash — the gateway's login page strips only the key and forwards the
 * rest of the fragment).
 */
export function toPublicLocation(
  publicUrl: string,
  loc: { pathname: string; search: string; hash: string },
  password: string | null = null,
): string {
  let url = publicUrl + loc.pathname + loc.search + loc.hash;
  if (password) {
    const key = "veld-key=" + encodeURIComponent(password);
    url += (loc.hash ? "&" : "#") + key;
  }
  return url;
}

/** Toolbar action: copy this page's public URL, or explain how to get one. */
export async function copyPublicUrl(): Promise<void> {
  let list: SharesList;
  try {
    const r = await fetch("/__veld__/api/shares");
    if (!r.ok) throw new Error(String(r.status));
    list = (await r.json()) as SharesList;
  } catch (_) {
    toast("Could not reach Veld to look up the public URL");
    return;
  }

  const target = findPublicUrl(list, window.location.hostname);
  if (!target) {
    toast("Not shared to the web yet — run: veld share --web");
    return;
  }

  const url = toPublicLocation(
    target.publicUrl,
    window.location,
    target.password,
  );
  const label = target.password
    ? "Public one-link copied (carries the viewer password)"
    : "Public URL copied";
  try {
    await navigator.clipboard.writeText(url);
    toast(label);
  } catch (_) {
    // Clipboard can be denied (permissions policy); still give the user the
    // URL rather than a dead end.
    toast("Copy failed — public URL: " + url);
  }
}

// "Copy public URL" — translate the current browser location to its public
// gateway URL (SHARING_V2.md §5.6): swap the host for the share's minted
// public host, keep path + query + hash so a deep link to the current screen
// survives the copy.
//
// The daemon's share list is fetched on demand (no polling): the overlay is
// served same-origin under /__veld__/, which Caddy proxies to the daemon.
import { toast } from "./toast";

interface GatewayPublicUrl {
  node: string;
  hostname: string;
  public_url: string;
}

interface ShareInfo {
  public_urls?: GatewayPublicUrl[];
}

interface SharesList {
  shares?: ShareInfo[];
}

/** The public URL for `hostname` among the daemon's active web shares. */
export function findPublicUrl(
  list: SharesList,
  hostname: string,
): string | null {
  for (const share of list.shares ?? []) {
    for (const u of share.public_urls ?? []) {
      if (u.hostname === hostname) return u.public_url;
    }
  }
  return null;
}

/** `public_url` + the location's path, query, and hash. */
export function toPublicLocation(
  publicUrl: string,
  loc: { pathname: string; search: string; hash: string },
): string {
  return publicUrl + loc.pathname + loc.search + loc.hash;
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

  const publicUrl = findPublicUrl(list, window.location.hostname);
  if (!publicUrl) {
    toast("Not shared to the web yet — run: veld share --web");
    return;
  }

  const url = toPublicLocation(publicUrl, window.location);
  try {
    await navigator.clipboard.writeText(url);
    toast("Public URL copied");
  } catch (_) {
    // Clipboard can be denied (permissions policy); still give the user the
    // URL rather than a dead end.
    toast("Copy failed — public URL: " + url);
  }
}

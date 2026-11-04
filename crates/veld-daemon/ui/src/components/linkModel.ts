import type { PaneIcon, WorktreeLink } from "../api";

export type LinkKind = "pr" | "preview" | "issue" | "doc" | "link";
export type LinkTone = "neutral" | "info" | "success" | "warning" | "danger";

/** Display order of the kinds, which is also the order slots are arranged in. */
export const KIND_ORDER: readonly LinkKind[] = ["pr", "preview", "issue", "doc", "link"];

/** Least to most severe: a tone's index is its rank. */
const TONES: readonly LinkTone[] = ["neutral", "success", "info", "warning", "danger"];

const DEFAULT_GLYPH: Record<LinkKind, string> = {
  pr: "git-pull-request",
  preview: "rocket",
  issue: "circle-dashed",
  doc: "book",
  link: "link",
};

const PLURAL: Record<LinkKind, string> = {
  pr: "PRs",
  preview: "previews",
  issue: "issues",
  doc: "docs",
  link: "links",
};

/** A link with every closed-vocabulary field resolved to a value this UI knows. */
export interface NormalLink extends Omit<WorktreeLink, "kind" | "tone"> {
  kind: LinkKind;
  tone: LinkTone;
}

export interface LinkGroup {
  kind: LinkKind;
  links: NormalLink[];
}

export type Slot =
  | { type: "single"; link: NormalLink }
  | { type: "group"; kind: LinkKind; links: NormalLink[]; tone: LinkTone }
  | { type: "overflow"; groups: LinkGroup[] };

/** At most this many slots are visible; more fold into one overflow slot. */
export const MAX_SLOTS = 3;

/** Unknown kinds read as `link`, unknown tones as `neutral`. */
export function normalizeLink(l: WorktreeLink): NormalLink {
  return {
    ...l,
    kind: (KIND_ORDER as readonly string[]).includes(l.kind) ? (l.kind as LinkKind) : "link",
    tone: (TONES as readonly string[]).includes(l.tone) ? (l.tone as LinkTone) : "neutral",
  };
}

/** The glyph a link is drawn with: its own, else its kind's default. */
export function linkGlyph(link: { kind: LinkKind; icon: PaneIcon | null }): PaneIcon {
  return link.icon ?? kindGlyph(link.kind);
}

export function kindGlyph(kind: LinkKind): PaneIcon {
  return { kind: "name", value: DEFAULT_GLYPH[kind] };
}

/** A link set on another branch than the worktree is on now describes work that
 *  may no longer be here. A link with no recorded branch is never stale. */
export function isStale(link: { branch: string }, currentBranch: string): boolean {
  return link.branch !== "" && link.branch !== currentBranch;
}

/** danger > warning > info > success > neutral. */
export function worstTone(tones: readonly LinkTone[]): LinkTone {
  let worst: LinkTone = "neutral";
  for (const t of tones) {
    if (TONES.indexOf(t) > TONES.indexOf(worst)) worst = t;
  }
  return worst;
}

/** "2 PRs", "3 previews", "4 links". */
export function groupLabel(kind: LinkKind, count: number): string {
  return `${count} ${PLURAL[kind]}`;
}

/**
 * Lay links out for the bar. Slots are per kind and independent of the total: a
 * kind with one link is a pill of its own, a kind with several is one group pill.
 * Within a kind the incoming order is kept (the daemon sends creation order). Past
 * `MAX_SLOTS` the first two stay and the rest fold into a single overflow slot.
 */
/**
 * A URL as `veld ide link` files it: scheme and host lowercased, the query,
 * fragment and trailing slash dropped (`normalize_link_url` in Rust). Two URLs that
 * normalize alike are the same link.
 */
export function sameLink(a: string, b: string): boolean {
  return normalizeUrl(a) === normalizeUrl(b);
}

function normalizeUrl(url: string): string {
  const bare = url.trim().split(/[#?]/u)[0];
  const at = bare.indexOf("://");
  if (at < 0) return bare;
  const rest = bare.slice(at + 3);
  const slash = rest.indexOf("/");
  const host = slash < 0 ? rest : rest.slice(0, slash);
  const path = slash < 0 ? "" : rest.slice(slash).replace(/\/+$/u, "");
  return `${bare.slice(0, at).toLowerCase()}://${host.toLowerCase()}${path}`;
}

/**
 * The links the top bar shows: every one but those a project badge already links
 * to. The badge is the live view of that page, so the link would only repeat it —
 * but it stays pinned, because the rail shows only pinned pull requests.
 */
export function withoutBadged(
  links: readonly WorktreeLink[],
  badgeHrefs: readonly string[],
): WorktreeLink[] {
  return links.filter((l) => !badgeHrefs.some((h) => sameLink(l.url, h)));
}

export function arrangeLinks(links: readonly WorktreeLink[]): Slot[] {
  const byKind = new Map<LinkKind, NormalLink[]>();
  for (const raw of links) {
    const l = normalizeLink(raw);
    const list = byKind.get(l.kind);
    if (list) list.push(l);
    else byKind.set(l.kind, [l]);
  }
  const slots: Slot[] = [];
  const groups: LinkGroup[] = [];
  for (const kind of KIND_ORDER) {
    const list = byKind.get(kind);
    if (!list) continue;
    groups.push({ kind, links: list });
    slots.push(
      list.length === 1
        ? { type: "single", link: list[0] }
        : { type: "group", kind, links: list, tone: worstTone(list.map((l) => l.tone)) },
    );
  }
  if (slots.length <= MAX_SLOTS) return slots;
  return [
    ...slots.slice(0, MAX_SLOTS - 1),
    { type: "overflow", groups: groups.slice(MAX_SLOTS - 1) },
  ];
}

/** How many links an overflow slot holds. */
export function overflowCount(groups: readonly LinkGroup[]): number {
  return groups.reduce((n, g) => n + g.links.length, 0);
}

/** "set 3h ago", or "just now" inside the first minute. A timestamp that does not
 *  parse yields "". */
export function ageText(updatedAt: string, now: number): string {
  const t = Date.parse(updatedAt);
  if (Number.isNaN(t)) return "";
  const s = Math.max(0, Math.round((now - t) / 1000));
  if (s < 60) return "just now";
  const m = Math.floor(s / 60);
  if (m < 60) return `set ${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `set ${h}h ago`;
  return `set ${Math.floor(h / 24)}d ago`;
}

/** The host of a link, or the raw string when it is not a parseable URL. */
export function linkHost(url: string): string {
  try {
    return new URL(url).host || url;
  } catch {
    return url;
  }
}

/** The host plus path of a link, without scheme or query. */
export function linkHostPath(url: string): string {
  try {
    const u = new URL(url);
    return u.host + (u.pathname === "/" ? "" : u.pathname);
  } catch {
    return url;
  }
}

/** The pull requests a rail row speaks for: the worktree's own, so stale ones are
 *  left out, and the tone the row's marker may take from them. Only danger and
 *  success are loud enough to show; everything else reads as neutral. */
export function railPrs(
  links: readonly WorktreeLink[],
  currentBranch: string,
): { prs: NormalLink[]; tone: "danger" | "success" | "neutral" } {
  const prs = links
    .map(normalizeLink)
    .filter((l) => l.kind === "pr" && !isStale(l, currentBranch));
  const worst = worstTone(prs.map((l) => l.tone));
  return { prs, tone: worst === "danger" || worst === "success" ? worst : "neutral" };
}

/** The tooltip body for one link: its own tooltip, the host, and when and where
 *  an agent set it. */
export function linkTooltip(link: NormalLink, currentBranch: string, now: number): string {
  const age = ageText(link.updated_at, now);
  const setBy = age === "just now" ? "set by agent just now" : age.replace("set ", "set by agent ");
  const where = isStale(link, currentBranch) ? ` on branch ${link.branch}` : "";
  return [link.tooltip, linkHost(link.url), `${setBy.trim() || "set by agent"}${where}`]
    .filter(Boolean)
    .join("\n");
}

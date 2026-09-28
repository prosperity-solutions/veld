/**
 * Veld's feature highlights. See `model.ts` for what one is and when it shows.
 *
 * Keyed by slug so a call site names the one it anchors (`HIGHLIGHTS["…"]`), and a
 * typo is a type error instead of a bubble that silently never appears. Same id
 * rules as `promotions/content.ts`: never rename a slug, never reuse one.
 */

import type { Highlight } from "./model";

export const HIGHLIGHTS = {
  // On the marker picker's face switch, because that is where the choice is made
  // — in the New worktree dialog and in Change marker…, whichever somebody opens
  // first. The What's-new card `pixel-markers` says it exists; this says "here".
  "pixel-markers": {
    slug: "pixel-markers",
    since: "2026-09-28",
    title: "New: Pixels",
    body: "A small pattern in your worktree's colour. The easiest marker to tell apart when the sidebar is collapsed.",
  },
} as const satisfies Record<string, Highlight>;

export type HighlightSlug = keyof typeof HIGHLIGHTS;

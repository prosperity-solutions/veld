/**
 * The content. Two collections, one shape.
 *
 * **`IDENTITY`** is the front door — what Veld *is*, in three claims, shown to
 * anyone with no projects yet. It is not seen-tracked and never will be: it is
 * derived from "this user has zero projects", so it un-shows itself the moment
 * they import one and comes back if they later remove their last. A dismissable
 * first-run screen is how you strand somebody on a blank page, which is the
 * complaint this whole surface exists to answer.
 *
 * **`PROMOTIONS`** is the seen-tracked channel, in two kinds:
 *
 * - `onboarding` — orientation that does not go stale. Shown to anyone who has
 *   not read it, whenever they arrived.
 * - `news` — a change. Auto-read for a user who arrived after it, so a fresh
 *   install never gets a modal about last spring.
 *
 * Both carry `since`, the day the entry shipped. It is shown on every card; the
 * kind decides only whether it gates.
 *
 * Adding a `news` entry is a *decision with a cost*, not a step in a checklist:
 * these interrupt people. Promote a change that alters how someone works — a new
 * pane kind, a new way to move between worktrees, a default that flipped. Do not
 * promote a fix, a flag, a perf win, or a feature only the person who shipped it
 * will ever look for. See `docs/promotions.md`; `/ship` asks the question at the
 * point where the answer is actually known.
 *
 * **Write the outcome, not the mechanism.** The headline is what the reader can
 * now do, or stop doing — to them, in their words. The body says what changes
 * about their day, and only then where to look. Never open with "Veld now…", a
 * feature name, or a description of the UI: the reader does not want a feature,
 * they want their afternoon back.
 *
 *     ✗  Each worktree shows a glyph for what its terminals have to say.
 *     ✓  Walk away from a running agent. The worktree that needs you says so.
 *
 * The check that catches it: if the sentence would still read as true to
 * somebody who will never use the feature, it is describing the product instead
 * of their day. Rewrite it starting from "you".
 *
 * Two rules with teeth, both of them about ids:
 *
 * - **An id is never renamed and never reused.** It is the string the daemon
 *   persists state against. Renaming re-promotes the entry to every existing
 *   user; reusing a retired one silently suppresses the new promotion for
 *   everyone who saw the old one.
 * - **Retiring is deleting.** An id that disappears from this file is inert for
 *   every user who has state for it. Delete entries once they have stopped being
 *   news — otherwise their copy sits in the single-file bundle forever.
 */

import type { Promotion, Section } from "./model";

export const IDENTITY: Section[] = [
  {
    id: "identity-agent-native",
    eyebrow: "Agent-native",
    headline: "No code editor. Deliberately.",
    body: "Your agent writes the code. Veld runs everything around it — terminals, dev servers, git worktrees, and the browser you check them in.",
    glyph: "terminal",
  },
  {
    id: "identity-parallel",
    eyebrow: "Parallel by default",
    headline: "Five branches, none of them waiting.",
    body: "Every git worktree keeps its own panes — terminals and live browser views, side by side — so changing task never means tearing one down.",
    glyph: "panes",
  },
  {
    id: "identity-devtools",
    eyebrow: "Dev tools, promoted",
    headline: "Test it small, then test it real.",
    body: "Put a phone-sized viewport next to your terminal, then share the running app to an actual device with one link.",
    glyph: "device",
  },
];

export const PROMOTIONS: Promotion[] = [
  {
    id: "whats-new-channel",
    kind: "onboarding",
    since: "2026-08-12",
    eyebrow: "Good to know",
    headline: "You won't miss what changed",
    body: "Rare cards, only when something changes how you work — never fixes or flags. Missed one? Reopen it from What's new in the project ⋯ menu.",
    glyph: "inbox",
  },
  {
    id: "worktree-inbox",
    kind: "news",
    since: "2026-08-12",
    eyebrow: "New",
    headline: "Stop watching panes that don't need you",
    body: "Walk away from a running agent or a long build. When one needs you, fails, or finishes, its worktree says so — and points at the tab it happened in.",
    glyph: "terminal",
  },
];

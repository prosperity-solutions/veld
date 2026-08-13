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
 * **`PROMOTIONS`** is the seen-tracked channel, and every entry in it is **a
 * change that landed on a day**. `since` is that day, it is shown on the card, and
 * it gates: a user who arrived after it never sees the entry, so a fresh install
 * never gets a modal about last spring.
 *
 * There was briefly a second kind — evergreen orientation, no date gate — and it
 * is gone. Without the gate nothing ever stops such a card reaching people, which
 * makes it the only shape that can rot into a tip-of-the-day nobody reads. What Veld
 * *is* belongs in `IDENTITY` above, which is derived from state and un-shows itself.
 *
 * Adding an entry is a *decision with a cost*, not a step in a checklist:
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
 *
 * **Append new entries to the end of `PROMOTIONS`.** Everything ships on the day
 * it ships, so a release adding two cards has two cards sharing a `since` — and
 * the history view breaks that tie by reverse array order, which makes the last
 * element the newest thing. An entry inserted into the middle therefore reads as
 * older than the card below it (measured: second of three, under an older card).
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
    id: "worktree-inbox",
    since: "2026-08-12",
    eyebrow: "New",
    headline: "Stop watching panes that don't need you",
    body: "Walk away from a running agent or a long build. When one needs you, fails, or finishes, its worktree says so — and points at the tab it happened in.",
    glyph: "terminal",
  },
  {
    // A trust primer as much as a feature announcement: the first time a card
    // labelled with a project's name appears in this dialog, the reader should
    // already know that somebody other than Veld can push one through. It is
    // still news — it landed on a day, and the people who need telling are the
    // ones who were already here. A fresh install after this date meets project
    // cards as an ordinary part of the channel, with the attribution on every one.
    id: "project-news",
    since: "2026-08-12",
    eyebrow: "From your team",
    headline: "Hear it from your team, not a failure",
    body: "When somebody changes how your project runs, their note reaches you once — marked with the project's name, and kept in What's new if you close it.",
    glyph: "inbox",
  },
  {
    id: "browser-find-in-page",
    since: "2026-08-13",
    eyebrow: "New",
    headline: "Search available for embedded browsers",
    body: "Find text on the page you're viewing without leaving Veld — see the match count and step through hits with Enter or the arrows.",
    glyph: "panes",
  },
  {
    id: "focus-mode",
    since: "2026-08-13",
    eyebrow: "New",
    headline: "Go quiet for a while, miss nothing",
    body: "One click in the top bar and the bell, the toasts, and the OS banners stop. The worktree rail still shows what happened while you were quiet.",
    glyph: "inbox",
  },
];

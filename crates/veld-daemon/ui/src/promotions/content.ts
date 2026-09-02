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
    // The button is visible in the top bar; what it *covers* is not. Nobody
    // guesses that it holds through a closed lid, and — the reason this card
    // exists at all — nobody would connect a dead overnight run on a MacBook to
    // `veld setup privileged`. That is the half a reader cannot find alone.
    id: "keep-awake",
    since: "2026-08-13",
    eyebrow: "New",
    headline: "Close the lid, agent keeps running",
    body: "Hold this machine awake from the cup beside search — 30 minutes, a few hours, or no limit. On a Mac on battery a shut lid also needs veld setup privileged.",
    glyph: "device",
  },
  {
    id: "focus-mode",
    since: "2026-08-13",
    eyebrow: "New",
    headline: "Go quiet for a while, miss nothing",
    body: "One click in the top bar and the bell, the toasts, and the OS banners stop. The worktree rail still shows what happened while you were quiet.",
    glyph: "inbox",
  },
  {
    // Two things nobody would find on their own, which is what earns the card. A
    // notification that crosses projects only shows itself when it happens — and
    // until now it arrived titled "Veld" and did nothing when clicked. The project
    // column ships **off**, so without a mention it is a surface that exists for
    // nobody. The selector's own changes are discoverable on the first click and
    // are not why this is here. `inbox`, like the two other cards about being told
    // something.
    id: "multi-project-parallelism",
    since: "2026-08-13",
    eyebrow: "New",
    headline: "Handle multiple projects better in parallel",
    body: "An agent waiting in a project you are not looking at says so, and clicking through takes you there. Turn on the project column for ⌘1…⌘9.",
    glyph: "inbox",
  },
  {
    // A **default that flipped**, which is the category this channel exists for:
    // it happens *to* the reader, with no action of theirs. The surface that
    // earns the card is the **in-page overlay**: it starts shares without ever
    // opening a Veld window, and unlike `veld share` — which prints an `Awake:`
    // line — it says nothing about the hold at all.
    //
    // A second card about keep-awake two days after `keep-awake` is real budget
    // spent, and it is spent knowingly: default-on *and* silent is the one
    // combination that cannot be defended. It also reaches two cohorts the
    // earlier card never did — anyone who installed on or after its `since` (the
    // date gate auto-reads it) and anyone who dismissed rather than read it.
    //
    // Framed from what the reader can now stop doing. Not "Veld now arms
    // keep-awake while sharing", which is the mechanism, and not a scene about
    // a demo dying mid-call, which is a beat of drama in place of a capability.
    id: "keep-awake-while-sharing",
    since: "2026-08-14",
    eyebrow: "New",
    headline: "Share a link and walk away",
    body: "This machine stays awake while you're sharing, and lets go when the share ends. Two hours on mains, half an hour on battery — both in Settings → Keep awake.",
    glyph: "device",
  },
  {
    // **A new way to move around**, which is the category with the clearest
    // claim on this channel — and one nobody discovers by looking, because a
    // keyboard shortcut has no affordance on screen to notice. The whole set
    // earns one card, not one per chord: what changes about the reader's day is
    // that Veld is drivable from the keyboard at all, and the individual chords
    // are what the overview is for.
    //
    // The id is free despite an earlier attempt using it: that card was reverted
    // inside its own PR (#315) and never reached a release, so no daemon has
    // ever persisted state against it. Its copy — "Navigate faster thanks to
    // more shortcuts" — is also the failure this file warns about: "more
    // shortcuts" is a fact about the product, and reads as true to somebody who
    // will never press one.
    //
    // ⌘/ rather than "the ⋯ menu → Shortcuts": pointing at a shortcut with a
    // shortcut is the one place that reads as a demonstration rather than a
    // direction, and it is the chord most worth keeping.
    id: "keyboard-shortcuts",
    since: "2026-08-18",
    eyebrow: "New",
    headline: "Drive Veld without reaching for the mouse",
    body: "⌃Tab steps tabs, ⌥Tab steps worktrees, and Shift reverses both — from inside a terminal or a browser pane too. Press ⌘/ for the full list.",
    glyph: "panes",
  },
  {
    // **A new capability with daily reach, and no way to find it.** The test in
    // this file's header is "would they otherwise find it" — and here the answer
    // is emphatically no in a way that is worse than usual: both gestures
    // *silently did nothing* before. Somebody who tried ⌘V with a screenshot
    // once, or dropped a file on a pane once, learned that Veld does not do that
    // and will never try again. A card is the only thing that reaches them.
    //
    // **Scope is the whole ⌘ family, not just the image.** The line-editing chords
    // on their own are a fix — a chord that should always have worked — and a fix
    // is not promotable. Bundled with the paste and the drop they stop being a fix
    // and become the thing the card is actually about: a pane now behaves like the
    // terminal you came from, so the habits you already have transfer.
    //
    // The headline is the maintainer's, chosen over "Your terminal's ⌘ keys are
    // back" with the objection on the table and overruled deliberately: it names
    // nothing, so by this file's own check ("would it read as true to somebody who
    // will never use the feature") it describes the product rather than the
    // reader's day, and the body carries the whole informational load. Recorded
    // because the next person to write a card here will read these as examples —
    // this one is a judgment call, not the pattern to copy.
    id: "terminal-file-input",
    since: "2026-08-18",
    eyebrow: "New",
    headline: "Terminal convenience is back",
    body: "⌘←/⌘→/⌘⌫ edit the line the way they do everywhere else, and ⌘V or a dropped file hands an agent the picture or the path itself.",
    glyph: "terminal",
  },
  {
    // **A new capability, and the reader has no way to discover it.** Before this,
    // reading a file an agent wrote meant leaving for Chrome — so the habit people
    // have is "switch apps", and nothing in the IDE would ever tell them to stop.
    // That is the "would they otherwise find it" test answered no.
    //
    // **The headline names the capability, at the maintainer's direction, with this
    // file's own outcome-first rule on the table and overruled deliberately.** By
    // that rule it should read as something the reader can stop doing ("Stop
    // leaving Veld to read a file", which is what this card said first). The
    // objection to the version that shipped is the documented one: it would read as
    // true to somebody who will never use the feature, so it describes the product
    // rather than the reader's day, and the body carries the whole informational
    // load. Recorded because the next person to write a card here will read these
    // as examples — this is a judgment call, like `terminal-file-input` above, not
    // the pattern to copy.
    id: "local-files-in-panes",
    since: "2026-08-20",
    eyebrow: "New",
    headline: "Open local HTML files in a browser pane",
    body:
      "The deck or report your agent just wrote opens beside the terminal: run open deck.html, or pick it from the recent list. It reloads when the file changes.",
    glyph: "panes",
  },
  {
    // A judgment call, and the reasoning rather than just the verdict, because
    // "no" is the expected answer here and this is a narrow audience: only
    // somebody building a mobile layout against the safe area wants it. Promoted
    // anyway on the two tests the doc sets — it is a capability that did not exist
    // (the variables read `0px` at every preset before this), and it lives inside
    // a menu, so the person who wants it would conclude Veld cannot do it rather
    // than find it. `browser-find-in-page` is the precedent for a browser-pane
    // capability earning a card.
    //
    // **"Safe area" throughout, never "notch" or "bezel"**, even though the notch
    // is what the gutters are *for* and the docs use the word freely to explain
    // the landscape asymmetry. Here it would mislead: those words describe drawn
    // device chrome, which this deliberately does not add (see
    // `browserViews.js` — a native view paints over any DOM inside its own rect,
    // so a drawn notch would need the view's rect to shrink, changing the very
    // viewport under test). A card promising a notch and delivering four CSS
    // variables spends the reader's one-time attention on a misunderstanding.
    // "Safe area" is also the name of the thing they will search for.
    //
    // The headline is otherwise the flattest true sentence that still starts from
    // the reader. "Stop guessing where the safe area is" was the first draft and
    // is the mini-narrative the doc names: a struggle and a relief, where the
    // capability alone reads faster.
    id: "browser-safe-area-insets",
    since: "2026-09-02",
    eyebrow: "New",
    headline: "Test a safe-area layout without a phone",
    body:
      "A header or bottom bar pinned to the safe area can be checked without a handset — emulate a phone and the page reads the same insets a real one reports.",
    glyph: "device",
  },
];

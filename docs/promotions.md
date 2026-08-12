# Feature promotions — how Veld tells you something changed

Two surfaces in the Veld IDE share one content vocabulary:

- **The first-run screen** (`/ide` with no projects imported) — what Veld is, in
  three claims, and the one thing to do about it.
- **What's new** — short cards for changes worth interrupting somebody over,
  shown once, and revisitable from the project ⋯ menu (which carries an unread
  dot and a count while anything is outstanding).

They render the same atom through two layouts. This document is the contract for
adding to either.

## Should this be promoted?

**Almost always: no.** A promotion interrupts every user of Veld exactly once,
and that budget is spent whether or not the thing was worth it. The channel is
worth having only for as long as opening it is reliably worth the reader's
attention.

Promote a change when **it alters how somebody works** and they would not
otherwise find it:

- a new pane kind, or a new way to move between worktrees
- a default that flipped, or a workflow that now has a different shape
- something small with outsized reach — a keystroke that removes a daily step

Do **not** promote:

- a bug fix, a perf win, or a refactor — the release notes have these
- a new flag, config field, or CLI option — someone looking for it will find it
  in `README.md` and the docs; someone not looking for it does not want a dialog
- anything only the person who shipped it would go looking for
- a change nobody has to do anything differently because of

`/ship` asks this question at the point where the answer is actually known — the
end of the change, by the person who built it. "No" is the expected answer and
needs no justification.

## How to write one: the outcome, not the mechanism

This is the rule that decides whether the channel is worth opening, and it is the
one an agent gets wrong by default — because an agent has just spent an hour
inside the implementation, and the implementation is the most available thing in
its head.

**The headline is what the reader can now do, or stop doing.** To them, in their
words. **The body says what changes about their day**, and only then, if it
helps, where to look. A promotion is not a changelog entry with nicer type.

| | |
|---|---|
| ✗ | *The panes you weren't watching* — "Each worktree now shows one glyph for what its terminals have to say — needs you, failed, finished, working — and marks the tab it came from." |
| ✓ | *Stop watching panes that don't need you* — "Walk away from a running agent or a long build. When one needs you, fails, or finishes, its worktree says so — and points at the tab it happened in." |

Both describe the same feature. The first is a tour of the UI; the second is
permission to go and do something else.

**The check that catches it:** if the sentence would still read as true to
somebody who is never going to use this feature, it is describing the product
instead of their day. Rewrite it starting from *you*.

Three habits that produce the wrong version:

- Opening with **"Veld now…"** or **"You can now configure…"**. Both announce
  that a thing exists. Nobody wanted the thing; they wanted the outcome.
- Leading with a **feature name**. The reader has never heard it and will not
  learn it from a card. Name the situation instead, and let them meet the
  feature when they get there.
- **Listing the states, options, or fields.** Four enumerated glyph meanings is
  documentation. One sentence about walking away is news.

## Two kinds

```ts
{ kind: "onboarding", since: "2026-08-12" }   // orientation; does not go stale
{ kind: "news",       since: "2026-08-12" }   // a change; gated by that date
```

- **`onboarding`** is shown to anyone who has not read it, whenever they
  arrived. Use it for orientation that stays true.
- **`news`** is **auto-read** for a user whose first IDE session predates it, so
  installing Veld today never means a modal about something that changed last
  spring. It stays visible in the panel — auto-read means "not yours to catch up
  on", not "hidden".

**`since` is mandatory for both kinds** and is shown on every card. The kind
decides only whether that date *gates* the card, never whether it exists: an
onboarding item was still written on a day, and a reader catching up deserves to
know which. The gate compares the **UTC** day and is strictly *before* the
arrival day, so something shipped on release day still reaches the person who
installed that morning.

## Four states, and dismiss is not read

| State | How you get there | Prompts again? | Counts as unread? |
|---|---|---|---|
| unread | the default — no stored row | yes | yes |
| `dismissed` | Esc, the close button, the overlay | **no** | **yes** |
| `read` | the *Got it!* button, or closing the panel you opened yourself | no | no |
| auto-read | `news` predating your first session | no | no |

**Dismissing is not reading**, and keeping them apart is the point: clearing a
modal in the middle of something must not lose the card. It stops prompting and
stays on the ⋯ menu's count until it is actually read.

The merge is monotone server-side — `read` wins over `dismissed`, neither is ever
undone — which is also what lets two windows act on the same card at the same
moment with no compare-and-swap.

## Adding one

Content lives in `crates/veld-daemon/ui/src/promotions/content.ts`. Append to
`PROMOTIONS`:

```ts
{
  // Kebab-case, stable forever, and never one already in the file — see below.
  id: "browser-device-frames",
  kind: "news",
  since: "2026-09-04",          // the day it ships
  eyebrow: "New",               // <= 24 chars
  headline: "Check the phone layout without a phone",   // <= 44 chars
  body: "Size any browser pane to a device and keep it beside the terminal, so a layout bug shows up while you are still in the file.",
  glyph: "device",              // from the closed set in model.ts
}
```

`crates/veld-daemon/ui/src/promotions/model.test.ts` fails the build on a breach
of any cap, a malformed or missing id or date, or a duplicate — the caps are a gate, not
advice, because content is often written by an agent following a checklist and
nothing else in the toolchain can see that a headline ran long.

Two rules about ids, both of which fail silently rather than loudly:

- **Never rename an id.** It is the string the daemon persists state against. A
  rename re-promotes the entry to every existing user.
- **Never reuse a retired id.** The new promotion is suppressed for everyone who
  saw the old one.

**Retiring is deleting.** An id that disappears from `content.ts` is inert for
every user who has state for it — deletion costs nothing and is the intended end
of a promotion's life. Delete entries once they have stopped being news. The
`/ide` bundle is one self-contained HTML file, so copy that is never deleted
ships forever.

## Illustrations

Line art from the closed `GlyphName` set, drawn in `currentColor`. **No rasters,
no screenshots, no video** — the bundle inlines everything it ships, and Veld has
two themes and ships weekly, so a screenshot is wrong in one theme the day it
lands and wrong in both within a fortnight. If a promotion cannot be illustrated
by an existing glyph, prefer the closest one over growing the set.

## How delivery works

The daemon stores two things in `kv` and has no idea what either means:

- `promotions.state` — a map of opaque ids to `dismissed` / `read`. Unread is the
  *absence* of an entry, so the map stays proportional to what the user acted on.
- `promotions.firstUse` — when this user arrived. Stamped once, on the first
  client request, and **never overwritten**; the date gate is meaningless if
  "when did they arrive" drifts forward on every load.

  The stamp is **the earliest evidence the user predates now, not the clock** —
  the oldest registered repo when there is one, otherwise now. Reaching for
  `now()` alone looks obviously right and quietly breaks the cohort that matters
  most: every *existing* user meets this code for the first time on the day they
  upgrade, so "now" declares an eight-month user brand new, and the promotion
  shipped in that very release is dated before their "arrival" and auto-read for
  everyone who opens a day late. The channel would launch reaching almost nobody.

`POST /api/promotions/state` returns both (stamping `first_use` if absent);
`POST /api/promotions/mark` merges ids into a state. That is the whole server
surface. **The daemon never looks inside a promotion** — content, kinds, dates
and every decision made from them live in the UI bundle, exactly as a pane
layout's contents live entirely in the client. Adding a promotion is therefore a
UI-only change, and an older daemon serving a newer bundle keeps working. The
date gate in particular is computed client-side, because a daemon that filtered
by date would have to know that promotions have dates.

**Nothing may key any of this on database freshness.** The tempting version — "a
database with no rows is a new user" — is wrong here in a way that bites daily:
`veld start --preset dev` mints `.veld-dev/<run>/veld.db` several times a day,
and either the CLI or the daemon may be the process that creates one. Note the
asymmetry that makes the repo evidence above safe where freshness is not: a repo
row is proof the user *was* here, while an empty table proves nothing at all — so
the evidence only ever moves the stamp **backwards**, and a throwaway dev
database simply falls through to now and stays quiet.

To revisit anything — including auto-read items from before you arrived — open
**What's new…** from the project ⋯ menu. That shows everything the build ships
whatever its state, and closing it marks the set read.

## Why the first-run screen is not a promotion

`IDENTITY` is not seen-tracked and never will be. It is derived from "this user
has zero projects", so it un-shows itself when a project is imported and comes
back if the last one is ever removed. A dismissable first-run screen is how you
strand somebody on a blank page — the exact complaint this surface was built to
answer — and that it would only strand them once is not a defence.

The cap of three is the same kind of decision. A fourth aspect costs removing one
of the three, which is the only thing that keeps a front door good in a product
that ships weekly.

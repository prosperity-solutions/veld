# Feature promotions — how Veld tells you something changed

Two surfaces in the Veld IDE share one content vocabulary:

- **The first-run screen** (`/ide` with no projects imported) — what Veld is, in
  three claims, and the one thing to do about it.
- **What's new** — short cards for changes worth interrupting somebody over,
  shown once, and revisitable from the project ⋯ menu (which carries an unread
  dot and a count while anything is outstanding).

They render the same atom through two layouts. This document is the contract for
adding to either.

**There are two sources of cards, not one.** Veld's own live in this repo; a
project's live in its `veld.json`, and reach that project's team only. Everything
below applies to both unless it says otherwise — the vocabulary, the caps, the four
states and the storage are shared deliberately, and the differences are collected
in [A project's own news](#a-projects-own-news).

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

## One kind: a change, with the day it happened

```ts
{ since: "2026-08-12" }   // the day it shipped, shown on the card, and it gates
```

Every card is a change that landed on a day. `since` is mandatory, has no default,
and is **auto-read** for a user whose first IDE session predates it — so installing
Veld today never means a modal about something that changed last spring. It stays
visible in the panel: auto-read means "not yours to catch up on", not "hidden".
The gate compares the **UTC** day and is strictly *before* the arrival day, so
something shipped on release day still reaches the person who installed that
morning.

**There was a second kind — `onboarding`, evergreen orientation with no date gate —
and it was removed.** Worth recording why, because it will be proposed again:

- With no gate, **nothing ever stops such a card reaching people.** It is the only
  card shape that *can* rot, and it rots by default: the author who wrote it moves
  on, no reader has a reason to reread it, and what is left is a tip-of-the-day.
  A `news` card self-retires whether or not anyone tidies up.
- **Veld's own orientation has a better surface.** The first-run screen is derived
  from "this user has no projects", so it un-shows itself and comes back if they
  remove their last one. A seen-tracked evergreen card is a worse version of that.
- **It was the source of every UX problem** the channel had: sections in the
  history view, a date stamped on something evergreen, a stepped flow, and an
  ordering tie-break that only mattered because dated and undated cards shared a
  list.
- What it was reached for — a project's "how we work in this repo" — is standing
  practice, which belongs in the repo's own docs. `ide.quicklinks` can point at
  them. "We *changed* how we work here" is news like anything else.

The removal is structural, not a convention: `predatesUser` in `model.ts` is
unconditional, so a card whose audience never shrinks is now unrepresentable.

## Four states, and dismiss is not read

| State | How you get there | Prompts again? | Counts as unread? |
|---|---|---|---|
| unread | the default — no stored row | yes | yes |
| `dismissed` | Esc, the close button, the overlay | **no** | **yes** |
| `read` | the *Got it!* button, or closing the panel you opened yourself | no | no |
| auto-read | a card predating your first session | no | no |

**Dismissing is not reading**, and keeping them apart is the point: clearing a
modal in the middle of something must not lose the card. It stops prompting and
stays on the ⋯ menu's count until it is actually read.

The merge is monotone server-side — `read` wins over `dismissed`, neither is ever
undone — which is also what lets two windows act on the same card at the same
moment with no compare-and-swap.

## One presentation, two entrances

The panel that opens itself and the panel the ⋯ menu reopens are **the same list**.
What differs is scope and what closing means:

| | Auto-open | ⋯ → What's new… |
|---|---|---|
| Shows | what is outstanding | everything, any state |
| Layout | one list, newest first | identical |
| Filter by source | no — nothing to curate | yes, when a project is selected |
| Esc / ✕ / overlay | **dismissed**, still counted | read |
| *Got it!* | read | read |

**One flat list, and no headings over it.** Every card is a change that landed on a
day, so date order is the only grouping the reader needs — and a taxonomy above
three cards is a label to skip.

**A stepped flow was built here and removed**, and the reason is worth keeping.
Presenting orientation one card per page, then the news together, reads as a
first-run wizard — a different genre from "what's new", which every reader already
knows as a *list*. It made the two entrances look like two features, and its page
grouping ("one, then one, then two together") was arbitrary from outside: the
reader had to learn the surface before reading the content. (Removing the
`onboarding` kind later removed the thing the stepper was trying to present.)

**Scrolling: bound the list, not the dialog.** The card list is the scroll region
(`min(58vh, 560px)`), with the filter above it and the footer below it — the same
shape `NewWorktreeDialog` uses, which is the reason to copy it rather than
re-derive it. Three wrong versions shipped past review first:

- A Mantine `ScrollArea` *inside* an unbounded modal body gives **two scrollbars
  side by side**, because Mantine's modal body already scrolls.
- Making the body a fixed-height non-scrolling flex column fixed that and **clipped
  the filter row off the top** of the dialog.
- `position: sticky` on the footer fixed *that* and left the button **floating over
  the cards**, with the body's own scrollbar pinned to the dialog's outer edge.

Bounding the list is the version with one scrollbar, inset from the edge, and
nothing overlapping anything. A sticky child of a scrolling ancestor sticks to the
wrong box — which is what the older dialog's comment already said.

## Adding one

Content lives in `crates/veld-daemon/ui/src/promotions/content.ts`. Append to
`PROMOTIONS`:

```ts
{
  // Kebab-case, stable forever, and never one already in the file — see below.
  id: "browser-device-frames",
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

## A project's own news

A repo can tell its own team something changed, through this same channel:

```jsonc
// veld.json — or any file an `include` glob picks up, e.g. veld.d/news.jsonc
"ide": {
  "news": [
    {
      "id": "one-command-tests",
      "since": "2026-08-12",          // required; it also decides who sees this
      "eyebrow": "Heads up",
      "headline": "Stop guessing which test script works",
      "body": "The wrappers are gone — `just test` runs everything, and your old local alias is the one thing that will still fail today.",
      "glyph": "terminal"
    }
  ]
}
```

Merge the card with the change it describes. A teammate pulls, and the next time
they open the IDE they are told once.

Everything in this document up to here still applies — **especially "Should this be
promoted?" and "the outcome, not the mechanism"**, which matter *more* here than
for Veld's own cards. A repo author is writing about their own implementation, and
the implementation is the most available thing in their head.

### What differs from Veld's own cards

| | Veld's | A project's |
|---|---|---|
| Where it lives | `content.ts`, in the bundle | `ide.news` in `veld.json` |
| Ships when | a Veld release | a merge to the project's `main` |
| Reaches | every Veld user | that project's team |
| Arrival compared against | `promotions.firstUse` | the repo's `created_at` — when *this user imported the project* |
| Id stored as | the bare slug | `proj:<hash>:<slug>` |
| Attribution | the wordmark | the project's name |
| Cap | reviewed by a human | **5 live items**, enforced |
| Can be turned off | no | yes — `ui.showProjectNews` |

Four of those are load-bearing enough to say why.

**Only the main checkout's news counts.** The daemon reads `ide.news` from the
repo's *main* worktree — the primary clone — and discards every other checkout's
copy. So a card being drafted **in a worktree** cannot prompt anybody until it
lands, and a repo with five worktrees cannot put the same card in front of somebody
five times. The cost is that news is silent until main is pulled, which the top
bar's "update main" control already drives.

Note what this is keyed on, because the shorter version of the sentence is wrong:
it is the main **checkout**, whatever that checkout currently has *checked out* —
not the default branch. Veld reads the working tree, and `GET /api/repos` is polled
by every window and may not spawn `git`, so there is nothing there that could read
`main:veld.json` instead. Draft a card on a branch in the primary clone and you
will see your own card. That is the honest boundary: worktrees are isolated, the
main clone is not.

**Ids are namespaced, and the namespace is the repo's path.** `proj:<hash>:<slug>`,
where the hash is FNV-1a over the main-checkout root. Two unrelated repos both
shipping `new-build` therefore never collide, and neither can collide with one of
Veld's — kebab-case admits no `:`, so a Veld id can never be written in namespaced
form (`a_veld_promotion_id_can_never_occupy_a_namespace` fails if that is ever
loosened). Moving a checkout on disk re-shows that project's live cards once,
which is the same thing that already happens to its machine-var overrides, runs,
logs, stats and feedback — all keyed by a raw path. A loud, bounded, self-healing
failure was preferred to author-chosen global ids, whose cross-repo collisions are
silent and permanent.

**A project's arrival is when the user imported the repo**, not when they first
opened Veld. That is what makes a teammate who has had the repo for a year and a
new hire who cloned it this morning get different answers about a card dated six
months ago. Remove-and-reimport resets `created_at`, moving arrival *forward*,
which makes the back-catalogue `auto-read` — silent rather than a stack of modals,
which is the right direction for somebody who has just imported a repo.

Note that this **inverts the bias** of Veld's own stamp. `firstUse` reaches
backwards, biasing toward noise, because a launch reaching nobody is worse. A
channel a teammate can push a modal through biases the other way. Same gate,
opposite safe direction, and the reason is who is holding it.

**The caps are the mitigation.** A news channel in a shared config file is a
channel every teammate can push a modal through to every other teammate, and the
honest technical mitigations are the ones that also improve authoring: the copy
limits (24 / 44 / 160) stop a wall of prose, and `MAX_NEWS_ITEMS = 5` stops a
stack. Retiring is deleting, here as everywhere. Items over the cap, and any
malformed entry, are dropped with a `veld lint` warning — never a load error,
because a config that will not load takes `veld stop` and `veld logs` with it.

Over the cap it is the **oldest** entries that go, and that direction is
load-bearing: authors are told to append, and the history view breaks a shared day
by reverse array order, so the *last* entry is the newest thing. Refusing items once
the cap was reached — the first version of this — dropped precisely the card that had
just landed, the only one with an audience, and reported it through a lint nobody
runs on a pull.

**A `since` in the future is refused.** It is the one typo that re-creates the
never-expiring card: the date is the only thing that retires an item, so a day that
has not happened yet is after every arrival, forever — including everyone who joins
later. `2062` for `2026` is one keystroke. Both halves check it: `parse_news` drops
the entry and tells the author, and `projectCards` drops it again client-side for a
reader whose daemon predates that check.

Deliberately **not** built: a rate limit (whose clock? and three cards in a
migration week is legitimate) and per-project opt-in (consent would have to be
given before the first card, i.e. before there is anything to consent to). The
escape hatch is the user's own `ui.showProjectNews`, which is on by default and
hides the cards without marking them read.

### Attribution is a trust requirement

Every card in a mixed panel says who wrote it, and it takes **three** signals,
because the first two were each individually insufficient:

- **Veld's cards read `V.` – Official veld news.** The mark alone was too quiet to
  read as a claim; the words alone would not survive a repo *named* `veld` — this
  repo is one — so the mark carries the distinction and the words carry what the
  mark means. The **icon** mark, not the wordmark: a byline is one 10.5px line, and
  `veld.` at that height is four letterforms competing with the sentence above them.
- **A project's card reads "News from your project – ‹name›" in a bordered pill**,
  a different kind of mark from a logo: a label attached to content rather than a
  stamp on it. Both bylines are whole phrases rather than one-word tags, because a
  bare "Official" beside a bare name only tells a reader who already knows what the
  two alternatives were. The name is a separate element from the phrase, so it
  truncates on its own and a repo cannot name itself into the middle of the claim.
- **A project's card drops the brand accent.** The green glyph tile and green
  eyebrow are Veld's colour, and repo-authored text wearing them is precisely the
  mistakability the byline exists to prevent. Measured on the real panel: with the
  byline as the only signal, every card still read as Veld's at a glance. Colour is
  legible before text is, which is why this is the signal that does the work.

This is also why the panel is titled "What's new" rather than "What's new in Veld",
and why the wordmark is no longer in its footer — a Veld mark over the whole panel
is Veld vouching for a teammate's sentence. It is the one `/ide` dialog that does
not carry the wordmark; `docs/branding.md` records the exception.

Repo-declared content is untrusted input: plain text, escaped, no HTML, no images,
no remote fetches. It renders in a page same-origin with the daemon API and with
PTY tickets. `parse_news` additionally refuses copy that contains control, bidi or
zero-width characters — the bidi controls because the line a project's card sits in
is the byline the reader is checking provenance against, and the zero-width ones
because `trim` does not remove them, so a card can otherwise pass a 24-character cap
and render as nothing.

**What attribution does not defend against, stated plainly:** a repo may write
"Official veld news" into its own `eyebrow`. Nothing filters copy by content, and
filtering it would be a homoglyph arms race. What survives that is the part the copy
cannot reach — a project's card has no `V.` mark, carries the "News from your
project" pill, and drops the brand accent, so the two are still different-looking
cards. The claim is "not mistakable at a glance", not "cannot contain a lie".

### The history view

Opening **What's new…** from the ⋯ menu shows everything either source ships,
whatever its state. It is a *browsing* surface, and it differs from the
interrupting one in two ways.

The *presentation* is not one of them: it is one flat list with no section
headings, identical to the interrupting panel's (see
[One presentation, two entrances](#one-presentation-two-entrances)).

**Newest first, and the tie-break matters.** Sorted by `since` — not by channel,
because "Veld's five, then the project's three" is two lists under one heading. On
a shared day, ties break by **reverse array order**: everything ships on the day it
ships, so a release adding two cards has two cards with one date, and both
`content.ts` and a project's `ide.news` are appended to. Without that, the newest
addition lands wherever it happens to sit in the array — measured: second of three,
beneath an older card. **Append new entries; do not insert them.**

**A filter by source**, offered whenever a project is selected — including a
project with no news, whose tab is present but **disabled**. "This project has told
you nothing" is an answer worth being able to see; a missing tab reads as a missing
feature. Closing marks the whole set read, not the filtered subset — a filter is a
way of looking at the list, not a way of leaving part of it unread.

### If four fields are not enough

They are meant not to be, sometimes. The designed extension point is a `details`
pointer to a repo-relative Markdown file, and it is deliberately **not** built yet:
build it when somebody actually hits the limit. If it is ever built it wants a
strict subset renderer and **never a Markdown library**, no HTML passthrough, no
images (the `/ide` bundle inlines everything it ships), and `https:` links shown
as their literal URL.

Until then: say the outcome in the card and let the reader find the detail where
detail belongs.

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
surface. **The daemon never looks inside a promotion** — content, dates
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

Because the ids are opaque, **a project's news needed no daemon endpoint at all.**
Its cards share this one state map under a `proj:<hash>:` namespace, and the only
server-side work the feature added is parsing `ide.news` out of `veld.json` — which
happens in `veld_core::ide` because that is the process that reads the config, and
is validated in `config::validate` rather than in the loader. `promotions.rs` still
does not know what a promotion is.

What the endpoint carries is bounded on purpose: a `mark` sends the app's own
manifest plus **the selected project's** news, never every imported project's.
`MAX_IDS = 256` bounds one *request*, not the stored row — the row grows
monotonically and the daemon cannot prune an opaque id.

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

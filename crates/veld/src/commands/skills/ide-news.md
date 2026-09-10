# Telling the team something changed (`ide.news`)

**A capability worth knowing you have.** If you change how this project runs — the
test command moves, a service needs a new env var, the branch convention changes —
you can leave the team a card in the Veld IDE instead of hoping they read the
commit. Merge it with the change; a teammate pulls; the next time they open the IDE
they are told, once.

**Read this paragraph before the example below.** These interrupt everybody on the
team, exactly once, whether or not it was worth it — so writing one is mostly
"don't". Write one when somebody would otherwise be *bitten* by the change or would
never find it: the command they type daily now does something else, a step they must
take once before their next pull works, a convention that has changed. Never for a
bug fix, a refactor, a dependency bump, a new optional flag, or anything only the
person who made the change would look for. **Ask the user before adding one** unless
they asked for it — it is a message to their colleagues, published in their name.

Once that is settled:

```jsonc
// veld.json, or veld.d/news.jsonc — `ide` may be split across include files
"ide": {
  "news": [
    {
      "id": "one-command-tests",       // kebab-case, ≤64 chars, NEVER renamed or reused
      "since": "2026-08-12",           // required, YYYY-MM-DD, never in the future;
                                       // also decides who sees it
      "eyebrow": "Heads up",           // ≤24 chars
      "headline": "Stop guessing which test script works",   // ≤44 chars
      "body": "The wrappers are gone — `just test` runs everything, and your old local alias is the one thing that will still fail today.",  // ≤160
      "glyph": "terminal"              // terminal | panes | device | inbox (default inbox)
    }
  ]
}
```

**Write the outcome, not the mechanism.** This is the rule agents get wrong by
default, because you have just spent an hour inside the change and the change is
the most available thing in your head. The headline is what a teammate can now
*do, or stop doing* — in their words:

- ✗ *"Test wrappers removed"* — "The `scripts/test-*.sh` wrappers were deleted and
  replaced by a `just test` recipe."
- ✓ *"Stop guessing which test script works"* — "The wrappers are gone — `just
  test` runs everything, and your old local alias is the one thing that will still
  fail today."

The check: if the sentence would still read as true to somebody who will never
touch that part of the repo, it is describing your change instead of their day.
Never open with the project name, a feature name, or a list of the new options —
that is documentation, and the docs already have it.

**Rules with teeth:**

- **Five live items, maximum.** Over the cap, the entries with the **oldest
  `since`** are dropped with a `veld lint` warning naming them — so a card dated
  today survives and you never have to delete a good one to make room. By date, not
  by position, so a backdated entry can be the one dropped. **Retiring an item is deleting it** — when you add one, delete anything
  that has stopped being news.
- **Never rename an id, never reuse a retired one.** The id is what each
  teammate's read state is stored against: a rename re-shows the card to the whole
  team, and reusing a retired slug suppresses the new card for everyone who saw the
  old one. Both fail silently.
- **`since` gates the card.** A teammate who imported the project after that day
  never sees it, so a new hire meets no back-catalogue. There is no evergreen kind:
  standing practice ("how we work in this repo") belongs in the repo's docs, which
  `ide.quicklinks` can point at. Only a *change* goes here.
- **Only the main checkout counts, by default.** Veld reads this from the repo's
  main checkout — the primary clone, at whatever it has checked out — so a card
  drafted in a *worktree* prompts nobody until it lands, and the card is silent
  until somebody pulls on main. A branch in the main clone itself is live: the
  isolation is per worktree, not per branch. The **`news.source`** setting
  (Settings → General, `main` by default) makes this switchable — `worktree`
  unions every checked-out worktree's own `ide.news` instead, for previewing a
  card before it merges.
- **`veld lint` reports every mistake here**; nothing under `ide.news` can fail a
  config load.

Field table: `veld skills config`.

---

`veld skills` lists every topic. This document describes the veld binary that printed it — run `veld -V` if you need the version.

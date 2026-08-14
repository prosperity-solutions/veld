# Extensions vision — config-driven customization for the Veld IDE

> This document is the reference point for the config-driven customization and
> extensibility capability of the Veld IDE, and it is the system's **long-term
> memory across sessions**. Four jobs: pin down where the line between **core**
> and **customization** is drawn; specify [the contract](#the-tier-1-contract)
> being built; hold the **backlog of extension features** the future system must
> serve; and keep a [decision log](#decision-log) of every fork that was expensive
> to reverse — *including the options that were rejected and why*. Agents and
> humans both update it — see [Rules for agents](#rules-for-agents).

## The vision

The Veld IDE becomes a platform that adapts to each project, driven by config
that lives in (or beside) the project. Today the IDE already adapts in small,
hard-coded ways — it reads `veld.json` for probes, presets, panes and actions,
and it reads per-project machine-overridable vars. The vision generalises that
into a first-class capability:

- **Custom badges, buttons and status icons** in named places of the IDE (the
  top bar, the worktree rail, the worktree detail, context menus), backed by
  commands that run on events, on an interval, or once.
- **Lifecycle hooks**: do setup when a worktree is created, teardown when it is
  deleted, react when a pane or a terminal starts.
- **Eventually, sandboxed scripts** (Figma-plugin-style) for extensions that
  outgrow what a command can express.

The IDE stops being a fixed set of screens; it becomes a set of *extension
points* that projects declare behaviour for.

## Where the line is: core vs customization

The question every new feature has to answer: **is this core Veld, or does it
belong in the customization layer?**

### The universal-primitive test

> Can Veld implement this using primitives that are identical no matter where
> the user's code is hosted — the git CLI, the filesystem, process execution,
> Veld's own orchestration model?

- **Yes → core.** Git operations qualify: `git` is the same command against
  GitHub, GitLab, Bitbucket, Gitea or a bare server. A feature that performs an
  operation through a universal primitive belongs in core.
- **No → customization.** If the feature needs a provider API, a
  provider-specific schema, or provider-specific auth, it belongs in the
  customization layer.

### The data-contract test (corollary)

Does Veld need to **understand** a data contract that varies by provider or by
project, or **perform** an operation expressible through a universal primitive?

| Feature | What Veld does | Verdict |
|---|---|---|
| Create a worktree from the latest `origin/main` | performs `git fetch` + `git worktree add -b … origin/main` | **core** |
| "Update main" (fetch + fast-forward the main checkout) | performs `git fetch` + `git merge --ff-only` | **core** |
| GitHub / GitLab PR status + CI checks in the top bar | must *understand* the provider's API schema and auth | **customization** |
| "Who last touched this file" inline blame | must *understand* a provider's API or a tool's output format | **customization** |

### Two rules that follow

1. **A configurable setting is not automatically an extension.** A policy knob
   on a *core* capability (e.g. "create worktrees from `origin` vs local `main`")
   is core, parameterized. The line is about whether the *capability itself* is
   provider-agnostic — not about whether it is configurable.
2. **The command is the abstraction boundary.** Provider-specificity lives
   behind a command; core never learns a provider's name. The future system's
   job is to let commands contribute UI (badges, buttons, status in named
   slots) and react to lifecycle events — nothing more. A "PR status" extension
   is a command that runs `gh pr view --json state,isDraft,statusCheckRollup`
   and prints a small provider-agnostic badge contract; Veld renders the badge.
   GitLab users ship `glab` instead. Core stays provider-blind.

## Capability tiers

The system will be built in layers. Each tier is a superset of the one below.

### Tier 0 — what exists today (the seed)

- `veld.json`: probes, presets, `actions` (`name`/`label`/`argv`, runnable via
  `veld action`), panes, `ide` blocks.
- Per-project machine-overridable vars (`veld config vars`).
- These are the proof that "config drives IDE behaviour" already works in a
  narrow, well-scoped form.

### Tier 1 — declarative command-backed extensions

The first real capability. Enough for the whole backlog below:

- **Named UI slots**: a fixed set of places extensions can contribute to (top
  bar per worktree, rail row, worktree detail, context menu). The slot set is a
  contract.
- **A badge/button contract**: extension declares "run this command, parse
  stdout as a badge/button spec, re-run on interval or on an event".
- **Actions**: buttons and menu items that invoke daemon commands, reusing the
  existing command-execution model (login-shell `PATH` injection, argv
  spawning, `X-Veld-Request` CSRF gate).
- **Lifecycle hooks**: daemon events extensions can subscribe to (worktree
  created/deleted, pane started, terminal started, run started/stopped).
- **Secrets as core infrastructure**: the *mechanism* for storing and exposing
  provider tokens is core and provider-agnostic; the *meaning* of a token is
  extension. Follows AGENTS.md: a secret is a pointer plus a sensitivity flag,
  never custody; never into a command line, a log, or a share payload.

The concrete shape Tier 1 is being built in — the config surface, the stdout
contract, the execution model and the security posture — is specified in
[The Tier 1 contract](#the-tier-1-contract) below, and the reasoning behind each
choice (including the alternatives that were rejected) is in the
[Decision log](#decision-log).

### Tier 2 — sandboxed scripts

Figma-plugin-style sandboxed JavaScript for extensions whose logic outgrows the
command contract: multi-step flows, real UI, stateful behaviour. The command
contract stays the safe default; the sandbox is for extensions that earn it.

### Design constraints that carry through

- **Repo-declared commands only** (AGENTS.md): a hook or command-executing
  extension may not originate from a fetched extension. Badges that merely
  render could be installed from elsewhere; anything that *runs a command*
  inherits the repo-declared rule. The "declared here" boundary must stay
  visible, never blurred.
- **No second config language** (AGENTS.md): extensions are declared in the
  project's config, in JSON/C, not in a template language compiled to JSON.
- **Diagnostics stay out of the loader** (AGENTS.md): semantic checks on
  extension declarations go in `config::validate`, never `parse_config`.

## The Tier 1 contract

This is the specification the implementation is built against. It is deliberately
narrower than the vision: **one slot, three extension types, no lifecycle hooks,
no sandbox.** What it fixes for good is the *vocabulary and the placement* — those
live in users' committed config files forever, so they are the expensive half.
Which extension types exist, and which slots, stays cheap to extend.

### Where extensions live

One flat collection under the existing `ide` block. **The slot is a field on the
item, not a level of structure**:

```jsonc
{
  "ide": {
    "extensions": [
      {
        "id": "pr",
        "slot": "topBar",
        "type": "status",
        "label": "PR",
        "argv": ["scripts/veld/pr-badge.sh"],
        "refresh_seconds": 60,
        "requires_bin": ["gh"],
        "when_missing": "hint",
        "hint": {
          "text": "Install the GitHub CLI to see this branch's pull request.",
          "href": "https://cli.github.com"
        }
      },

      // A group. It occupies the slot; its members do not.
      { "id": "open-in", "slot": "topBar", "type": "menu", "label": "Open in",
        "icon": "external-link", "items": ["webstorm", "vscode"] },

      // No `slot`, so it is declared but never rendered on its own — it is
      // reachable only by reference, from the menu above.
      { "id": "webstorm", "type": "action", "label": "WebStorm",
        "argv": ["webstorm", "${veld.root}"],
        "requires_bin": ["webstorm"], "when_missing": "hide" },

      { "id": "vscode", "type": "action", "label": "VS Code",
        "shell": "code '${veld.root}'",
        "requires_bin": ["code"], "when_missing": "hide" },

      // Referenced from the `pr` badge's stdout when no PR exists yet.
      { "id": "create-pr", "type": "action", "label": "Create pull request",
        "argv": ["gh", "pr", "create", "--web"], "requires_bin": ["gh"] }
    ]
  }
}
```

Fields common to every type: `id` (stable, safe identifier, unique per project),
`slot`, `align`, `type`, `label`, optional `icon` (from the existing pane-icon
allowlist), `requires_bin`, `when_missing`, `hint`.

**`align` picks the side of the slot**, `start` (default) or `end`. The top bar
already has a stated convention — *left is what this project does, right is what
the app does* — so a project's extensions default to the left cluster, and `end`
is the deliberate opt-out for something that reads as chrome rather than as
project state. It is a field rather than a `topBar.start` / `topBar.end` slot name
because a dotted selector is a path grammar, which was rejected for the same
reason elsewhere in this document. A slot with no meaningful sides ignores it, and
setting it there is a `validate` finding.

**`slot` is optional.** An item with a slot renders there; an item without one is
declared but never rendered directly, and is reachable only **by reference** —
from a `menu`'s `items`, or from a badge's stdout (see below). That is what lets
five editor actions exist without five buttons in a 42px bar.

**Anything that runs something takes `argv` *or* `shell`**, flattened exactly as
node-level `actions` already do — `argv` is spawned directly and is the default
recommendation, `shell` is the permanently-supported escape hatch, and the legacy
`command` alias comes along with the shared type. `${…}` interpolation reuses the
**pane variable context** minus its `pane.*` family — `${veld.root}`,
`${veld.branch}`, `${veld.branch_raw}` (unslugified, `argv` only — refused in
`shell`, see the 2026-08-13 decision-log entry), `${veld.worktree}`,
`${veld.project}`, `${veld.username}` — so extensions do not introduce a second
vocabulary. A reference outside that closed set is a `validate` finding, not a
badge that fails at spawn time.

Order within a slot is array order. An unknown `slot` or `type` is a non-fatal
`validate` finding and the item is ignored — the same leniency the rest of the
`ide` block already has, so a project can adopt a newer Veld's slot without
breaking on an older one. A reference to an id that does not exist, or to an item
of the wrong type, is likewise a `validate` finding, not a load failure.

### The three types

**`type: "status"`** — a badge. The daemon runs the command in the worktree root
and parses stdout as the badge contract:

```json
{
  "text": "PR #283 · merged",
  "tone": "success",
  "tooltip": "…",
  "href": "https://github.com/…/pull/283",
  "open_in": "system",
  "actions": [{ "id": "pr-checks", "label": "Watch checks" }]
}
```

`tone` is one of `neutral` / `info` / `success` / `warning` / `danger`. Three
tolerances make the simple case free and the failure case legible:

- stdout that is not the contract → its first line becomes `text`, tone
  `neutral`. So `argv: ["git", "rev-parse", "--short", "HEAD"]` is a working badge
  with no adapter at all.
- exit 0 with empty stdout → **nothing to show**, the badge is absent. This is how
  an extension says "not applicable here" without an error.
- non-zero exit → the badge renders in a failed state with the stderr tail as its
  tooltip. A broken extension is visible, never silent.

**`actions` are references, never commands.** An entry names the `id` of a declared
`type: "action"` extension in the same project; the daemon resolves it against the
on-disk config and runs *that* declaration's command. An optional `label`
overrides presentation only. This is the invariant the rest of the codebase already
holds — the browser sends a name, `run_action` looks the command up in config,
`resolve_pane` refuses a pane the config does not declare — extended one step: a
**runtime value may choose which declared action is offered, and may never
contribute one.** Without that rule a badge's stdout would be a command-injection
surface with no place to validate it, since there is no declaration to compare it
against.

That is what makes the flagship badge work properly: no PR yet → `{"text": "No PR",
"tone": "neutral", "actions": [{"id": "create-pr"}]}`; PR open → an `href` to it
plus whatever action is useful next.

Click semantics: `href` alone opens it; one action and no href runs it; anything
more opens a small menu.

**Where an `href` opens** is `open_in`: `system` (default) or `pane`. Declarable on
the extension and overridable per value in stdout. The default is the system
browser because an extension's `href` is, by construction, a *provider's*
authenticated web surface — a pull request, a CI run, a dashboard — where the user
is already signed in, and a Veld browser pane has its own cookie jar. That is the
opposite population from `ide.quicklinks`, which point at localhost and staging and
therefore belong in a pane; hence the opposite default. Only `http`/`https` are
accepted either way, matching the refusal quicklinks already applies to
`vscode://` and `file://`.

**`type: "action"`** — a button, or a menu member. Click runs the command; there is
no output contract and no refresh. Failure surfaces as a toast, the established
error surface for run diagnostics.

**`type: "menu"`** — a group. It occupies the slot and its `items` — id references
to declared `action` extensions — appear in a popover, so "Open in ▾" costs one
control instead of one per editor. Grouping is not cosmetic at this size: the top
bar carries around sixteen elements already, and a system that lets a project add
buttons without letting it group them makes the bar unusable at the third
extension.

A menu is itself an item, so ordering, `align`, `icon`, `requires_bin` and
`when_missing` work on it exactly as on anything else. A menu whose members are all
unavailable follows its own `when_missing` (default `hide`) rather than rendering an
empty popover.

Nesting is one level: a menu references actions, never other menus. Two levels of
popover in a 42px bar is a worse answer than a second menu.

### Availability, and why it is a teaching surface

`requires_bin` is resolved daemon-side through the same cached `PATH` lookup
`ide.panes` already uses for its own `requires_bin`. `when_missing` decides what an
unavailable item looks like:

| `when_missing` | Behaviour |
|---|---|
| `hide` | The item is not rendered. For optional tooling nobody should be nagged about (a specific editor). |
| `disable` | Rendered greyed, tooltip names the missing binary. |
| `hint` (default) | Rendered greyed and dashed with the `hint` text, and its `href` opens the install page. This is the newcomer path: a fresh clone *shows you* what the project expects you to have. |

**An explicit `when_missing` wins over the global `ui.hideDisabledActions`
setting.** That setting is about hiding inapplicable *core* actions; an extension
author choosing `hint` is teaching, and a user preference about clutter must not
silently delete the lesson.

### How a badge is evaluated

Round 1 is a **stateless, single-flight RPC**, not a daemon-side scheduler:

- The UI asks for the **currently visible worktree only**, over one CSRF-gated
  `POST`, on worktree switch and then on its own interval while the window is
  focused. Nothing is evaluated for the other 17 registered worktrees, and
  nothing is evaluated while no window is open.
- The daemon **remembers each badge's last value for its own `refresh_seconds`**,
  and the thing that prevents a stampede is *not* the absence of storage — it is
  that the lock is held across the child run. Three windows asking at once collapse
  to **one child process**: the second and third wait on the mutex and are then
  answered from the run the first made, with the value's age reported. A
  conventional TTL cache reads stale-and-returns on a miss, so all three would
  launch a refresh at the tick the TTL expired; here there is nothing to miss on.
  An **action** does not rate-limit against that memory, it invalidates it — a
  state change is not a repeated question — and it does so with a per-worktree
  timestamp rather than by taking the cells' locks, because those locks are held
  across a run.
- The response is the badge, or an explicit `failed`/`timeout` state. A run never
  hangs a response.

The migration path, when badges reach the worktree rail: rail badges are the case
where "evaluate what's on screen" stops working, because the rail shows every
worktree at once. That is when evaluation moves to a daemon-owned task keyed on
the **IDE ownership registry** (`ide.rs` already knows which worktrees a client
has open, and the socket is the lease, so there is no reaper to write) pushing
values over `/api/ide/channel`, with a last-known-good row in SQLite for a warm
first paint. Deliberately not built now — see the [Decision log](#decision-log).

### Security posture

A status extension is the **first thing Veld runs from a repo's config without a
user action**. Everything else needs a deliberate act: `veld start` for
`setup`/nodes/probes, a click for a pane or a node action. That difference is
real and it is the reason this section exists.

**There is no consent prompt, and that is a decision, not an omission.** The
provenance rule is satisfied — the command is declared *here*, in the repo's own
config, which is exactly what AGENTS.md requires. What is new is that execution is
unattended, repeated, and its output is squeezed into a dozen rendered characters.
So the budget goes on bounding and exposing execution rather than on asking:

**Say the consequence out loud, because it is not obvious:** by default (`main`,
the reversed decision below), declarations come from the project's **main
checkout**, so checking out an untrusted branch and selecting it in the IDE runs
only whatever that project's badge commands already are on main — not the
branch's own. Setting `extensions.source` to `worktree` restores the original
placement, where **checking out a branch and selecting it runs that branch's own
badge commands**; reviewing a fork pull request that way is an act of trust in
its author, the same way `veld start` on that branch already is. Two residuals
worth naming even under the `main` default: the command still *executes* with
the viewed worktree as its working directory, so a main-declared badge shelling
out to a repo-relative script still runs whatever that path resolves to in the
untrusted checkout — and "main" means the main *checkout*, not the default
*branch*, so a primary clone itself sitting on an untrusted branch (a `gh pr
checkout` run there) is the one case where `main` mode does not hold. Either
mode's placement is argued in the [Decision log](#decision-log); the lever for
somebody who wants the unattended half off entirely, regardless of source, is
`extensions.autoRefresh`, which stops it machine-wide and leaves clicks working.

- **`stdin` is closed and no tty is attached**, so a CLI that would prompt for
  credentials fails instead of hanging forever.
- **A hard timeout**, enforced by killing the **process group**, not the pid — and
  from a *drop guard*, so the kill cannot be skipped by the request future being
  dropped. Axum drops a handler when the client disconnects (a page reload, a closed
  window, a quit app), and a deadline that lives only on the awaited path leaves the
  repo's command running with nothing left to signal it. `kill_on_drop` alone is not
  enough either: it reaps the direct child, so a `shell` command's `sh` dies while
  whatever it forked keeps the pipe.
- **A byte cap on captured output, applied while reading** — the pipes are drained
  to EOF but at most 64 KiB is ever kept, so a badge that prints a log file is
  truncated rather than growing the daemon's heap. Draining rather than *stopping*
  at the cap is deliberate: an unread pipe fills, the child blocks in `write`, and
  a merely chatty command that would have exited fine becomes a 20-second timeout
  with no output at all. stdout is rendered as text — never as markup.
- **A minimum `refresh_seconds` floor (15s) and a cap on how many extensions a
  project may declare (24)**, both named constants in `veld_core::ide`, so the
  cost bound is set by Veld and not by a file in somebody's repo (the same
  reasoning as `PRESETS_EXPANDED_PER_LISTING`). A status run's deadline is 20s.
- **`NO_COLOR=1` / `TERM=dumb`** in the child environment, so a CLI that colours
  its piped output cannot corrupt the contract.
- **Every execution is logged with its full argv** to the daemon log.
- **A machine-global off switch** — the `extensions.autoRefresh` setting, on by
  default — disables all automatic evaluation while leaving buttons and menus
  clickable, because a click is the user asking. The answer for a cautious machine
  or a CI box, at zero cost to everyone else.
- **No secrets and no config-supplied `env`** in round 1. When provider tokens
  arrive they follow the AGENTS.md secret rule (pointer plus sensitivity flag,
  never into an argv or a log).

Two things were considered and **rejected**; both are recorded in the
[Decision log](#decision-log) with the argument that killed them. The short
version: a consent dialog bound to the declared commands re-prompts on every
`git pull` that touches `veld.json`, which manufactures exactly the
click-through reflex that makes prompts worthless — and a binary allowlist whose
first entry must be `gh` is an allowlist containing an authenticated arbitrary-HTTP
client.

### What round 1 deliberately does not do

- No lifecycle hooks. The reserved top-level `hooks` key is their home and
  predates this work; **a slot is a place in the UI and an event is not a place**,
  so forcing lifecycle into `ide.extensions` would be a category error. The
  *item vocabulary* (`type`, `argv`, `requires_bin`, …) is meant to be reused there
  verbatim, which is what keeps the two homes from drifting.
- No second slot (rail, worktree detail, context menu). `slot` exists so adding
  one is a string constant.
- No sandbox. `type` exists so `"script"` can arrive without restructuring.
- No `type: "link"`, and therefore **no migration of `ide.quicklinks` yet**.
  Quicklinks are static links in the browser pane's place list; folding them in
  means designing a third type for a surface nobody has complained about. They are
  the natural first consolidation once `type: "link"` earns its place.

## Decision log

A running record of the forks this system has hit, what was chosen, and what was
rejected. **The rejected options are the expensive part to regenerate** — the
first question anyone asks is "why not the obvious thing". Append; don't rewrite
history.

### 2026-08-12 — Placement: a flat collection with `slot` as a field

**Chosen:** `ide.extensions: [ { id, slot, type, … } ]`.

Rejected, with reasons:

- **`ide.extensions.topBar: [ … ]`** (a map of slot → array; the obvious answer).
  Creates a *second* key-dispatch surface inside `ide` whose first one already
  dispatches slot-ish things — `panes` and `quicklinks` live at `ide.*`, while
  `topBar`/`rail`/menus would live at `ide.extensions.*`. Which slot goes where
  becomes an arbitrary distinction frozen into committed user configs forever, and
  every future slot re-litigates it. With `slot` as a *value*, a new slot costs a
  string constant and no schema.
- **`ide.topBar: { status: [ … ], actions: [ … ] }`** (kind as structure). Models a
  left-to-right bar as unordered buckets, so an author cannot put a button between
  two badges; the fix is a numeric `order` field, which is a worse mechanism than
  the `type` tag it avoided. Also combinatorial: slots × kinds hand-written objects
  in a hand-maintained schema.
- **An `ide.commands` registry plus slots holding id lists.** Deduplicates
  *structure* rather than values, against the house rule, and imports a symbol
  table with dangling-reference validation into a config language that has already
  refused `extends` and mixins.
- **Top-level `extensions: { <name>: { requires, items: [{ at: "ide.topBar" }] } }`**
  (provider bundles). `at` as a dotted selector is a small second language, a map
  has no visual order, and it competes head-on with the reserved `hooks` key.
- **No discriminator, dispatching on the presence of `probe` / `activate` keys.**
  This was the sparring round's own top pick and was overridden: the
  hand-maintained JSON Schema cannot express which key combinations are legal
  without `oneOf` sprawl, and `PaneBody`'s `type` tag — introduced ahead of need,
  with one variant — is the established house precedent. Placement mechanism kept,
  discriminator restored.

### 2026-08-12 — Evaluation: stateless single-flight RPC, not a cache or a scheduler

**Chosen:** client asks for the visible worktree; daemon collapses concurrent
callers onto one child and holds no values.

Rejected, with reasons:

- **A TTL cache keyed by (worktree, extension) refreshed in the background when
  stale** (the obvious answer). With N independent pollers and no in-flight guard,
  every window reads stale at the same tick and all of them launch a refresh — an
  unbounded fan of children against a rate-limited API token, produced by a design
  whose purpose was to avoid duplicate work. Worse, a TTL has two states, so it
  cannot distinguish "never ran", "running now", "the CLI is waiting on a stdin
  nobody will write to", and "failed 40 minutes ago"; all four render as a stale
  value, and a badge that quietly stopped updating is the worst outcome a status
  indicator has.
- **A daemon-wide work queue with values in SQLite, delivered on the existing
  `/api/repos` poll.** Warmest first paint and simplest wire shape, but it spends a
  rate-limited token on all 18 registered worktrees to serve the one being looked
  at, forever, including overnight with the IDE closed. Its *delivery* half is the
  right answer later; its *trigger* half is not.
- **An actor per observed worktree driven by the IDE ownership registry, pushing
  over the control WebSocket.** This is where the system should end up and it was
  the sparring round's pick. Deferred because the registry's claim means
  *exclusivity* (one client arbitrates a worktree's pane layout) and badge delivery
  wants *multiplicity* (every window watching wants frames), so it needs its own
  non-exclusive subscribe frame — real work that buys nothing until a slot exists
  that shows more than one worktree at a time.
- **A long-running producer node whose stdout the daemon tails.** Moves
  scheduling, backoff and quota discipline into shell scripts, and hands the author
  the one failure nobody debugs correctly: a producer that is silently
  dead-but-alive, indistinguishable from a healthy one because silence is normal.
- **Invalidate on a filesystem event (`.git/HEAD`, `refs/heads/…`) instead of a
  clock.** Right instinct, wrong signal for the flagship example: PR state changes
  on a server — a review lands, CI goes red, someone merges — and none of that
  touches the filesystem.

### 2026-08-12 — Automatic execution: hygiene and transparency, not consent

**Chosen:** no gate. Bound execution (stdin closed, timeout, process-group kill,
output cap, interval floor, count cap), log every argv, ship one machine-global off
switch, and document plainly that registering a repository means trusting it to run
commands.

Rejected, with reasons:

- **One-time per-project consent listing the declared commands** (the obvious
  answer). It must re-prompt when the declaration changes, and `veld.json` changes
  on an ordinary `git pull` — so either it manufactures a click-through reflex, or
  it is decorative, because the adversary is the same actor who edits the file.
  There is no third option and no prompt wording fixes it. The decision also lands
  at the moment of *least* information, where the honest answer is always yes
  because the user just chose to work in this repo.
- **A machine-global allowlist of binaries permitted to run automatically.**
  Cheapest real gate, and its central promise is false for the exact binary the
  feature exists to run: allowlisting `gh` allowlists an authenticated
  arbitrary-HTTP client.
- **A closed registry of Veld-implemented providers** (`{ provider: "github-pr" }`),
  so the argv is written by Veld. Genuinely closes the attack surface, and kills
  the feature — a config-driven extension system whose commands Veld must implement
  in advance is a feature list. Keep it in reserve for a future *fetched* extension
  registry, where commands cannot be repo-declared by definition.
- **Sandbox profiles derived from the declaration** (Seatbelt / seccomp). The badge
  use case needs precisely the two capabilities whose combination is the whole
  attack — network and credentials — so the profile that makes a `gh` badge work is
  the profile an attacker asks for, at the cost of two sandbox implementations
  maintained forever.
- **Click-to-run on first render, automatic thereafter.** A consent prompt
  disguised as a badge: the reflexive click users learn to make is rewarded with an
  unattended loop, which is worse than either no gate or a real one.
- **Forbidding a repo-shipped script as `argv[0]`.** Proposed as hygiene and
  rejected on inspection: a project's badge adapter *is* a script in the repo, so
  the rule breaks the primary use case while buying nothing, since
  `["node", "-e", "…"]` is equivalent. The same reasoning keeps `shell` permitted —
  forbidding it while allowing `argv: ["sh", "-c", …]` is theatre.

### 2026-08-12 — Five corrections from the design review

The design above went to the maintainer before any code existed. Five changes came
back; all five are folded into [the contract](#the-tier-1-contract). Recorded here
because each one closes off an alternative.

- **`argv` *and* `shell`, from the shared flattened command type.** The original
  draft showed only `argv`. Diverging from the house rule here would have given
  extensions their own command vocabulary — the one thing this design is most
  concerned not to do.
- **A badge's stdout may offer actions, but only as `id` references.** The obvious
  reading of "let the script offer an action" is to let stdout carry an `argv`, and
  it is exactly wrong: a command arriving at runtime has no declaration to be
  validated against, which is the invariant `run_action` and `resolve_pane` both
  exist to hold. So the rule is **a runtime value may choose which declared action
  is offered, and may never contribute one.** The cost is dangling-reference
  validation, accepted once and then reused by menus.
- **`open_in` defaults to the system browser, not a pane.** The original draft sent
  every `href` to a browser pane, inherited from how `ide.quicklinks` behaves. Wrong
  population: a quicklink points at localhost or staging, an extension's `href`
  points at a provider's authenticated surface where the user is already signed in
  and a pane's separate cookie jar is not. Rejected alternative: routing extension
  hrefs through `ide.externalOrigins` / `route_url` and requiring projects to list
  `github.com` there — it makes the good outcome opt-in and the broken one the
  default for every fresh clone.
- **`type: "menu"` in round 1, not deferred.** Grouping was going to wait for a
  second round. It cannot: the bar carries ~16 controls already, and a system that
  lets a project add buttons without grouping them is unusable at the third
  extension. Its `items` are id references — the *same* mechanism the badge actions
  needed, which is why this costs almost nothing on top. Rejected alternatives:
  inline nested item objects (a second shape for the same thing, and it hides
  members from `validate`'s uniqueness check), and a `group: "open-in"` string tag
  (the group then has no declaration, so its own label, icon and `when_missing` have
  nowhere to live). Nesting is capped at one level.
  This does **not** revive the rejected `ide.commands` registry: there is still one
  flat collection, every item is still a full declaration, and keys still live at
  the use site. A menu references its siblings; it is not a second tier they live
  inside.
- **`align: "start" | "end"`.** Which side of the bar an extension sits on is the
  project's call, defaulting to `start` because the bar's existing convention is
  *left is what this project does, right is what the app does*. A field, not
  `slot: "topBar.end"` — a dotted selector is the path grammar rejected further up
  this log.

### 2026-08-12 — Seven corrections from driving the running feature

Everything in this round came from the maintainer using the built feature, which
is the class of finding no review subagent produces: colour, latency and glyph
legibility are not readable off a diff.

- **A first evaluation now renders.** The badge was absent until its command
  returned, so a `gh` call over slow wifi left a gap in the bar that was
  indistinguishable from a project declaring no badges. It shows the declared
  label with a spinner instead.
- **Tone colours moved off Mantine's palette onto the product tokens**, and the
  text is the tone **mixed 75% toward `--text`**. Measured, which is why the
  numbers are here: Mantine's `light` yellow put shade-6 text on a shade-0 fill,
  and even the bare product tokens are only 3.29:1 (`--warn`) and 4.36:1
  (`--danger`) as text on the light theme — both under 4.5:1. The mix clears it
  in both themes from one declaration. Rejected: darkening `--warn`/`--danger`
  themselves, which every other surface already depends on, and a second set of
  near-duplicate `-ink` tokens to keep in step.
- **`--info` is a new palette token.** There was no blue, so an `info` badge had
  to borrow `--live` and read as success — four tones pretending to be five.
- **The icon allowlist grew from 32 to 80 names** and is now shared by panes and
  extensions. Rejected: allowing *any* Tabler name via a dynamic `import()`. The
  bundle must contain every icon it can render, and per-icon code-splitting buys
  ~6000 names at the cost of a fetch-and-flash on first render plus a hard
  dependency on the package's internal file layout. A curated list that a test
  ties to the schema is worth more than exhaustiveness here.
- **A badge's *output* may name an `icon`**, overriding the declaration's, so a
  glyph can track state. No special syntax — it is one more field in the contract,
  resolved against the same allowlist, and an unknown name renders no glyph rather
  than failing the badge.
- **Refresh is a right-click on the badge**, for one or for all, using the same
  `mantine-contextmenu` the worktree rail's rows use. Rejected: putting Refresh in
  a left-click dropdown, which would cost *every* badge its one-click primary
  action ("open the pull request") to expose something wanted once a week. A
  forced refresh ignores `refresh_seconds`, bounded by a 3s floor so click-spam
  cannot fork a process per click, and it reports its own errors — unlike the
  background poll, which stays silent by design. **Running an action also forces a
  re-read**, because an action usually changes what a badge says.
- **A tone-demo extension was added and then removed.** It cycled all five tones
  one click apart, in this repo's own bar, because badge colour is the one part of
  this feature no test can check — a contrast ratio is arithmetic, "is this
  readable at 12px" is not. It paid for itself twice over: it is what surfaced the
  cache-invalidation defect below, and it settled the palette. Deleted once it
  had, rather than left in every contributor's bar as permanent scaffolding — but
  worth rebuilding the same way next time a purely visual property needs judging.

### 2026-08-12 — A badge is a Button, not a Badge

Reported as an alignment and corner-radius mismatch against the buttons beside it,
and the underlying cause was the element choice.

A Mantine `Badge` is a *label*. Using one for a control meant re-deriving, by hand
and slightly differently, everything the bar already levels for a button: height
(`--badge-height`), font size, weight, corner radius, and the vertical centring of
a leading glyph — which is why it sat as a 20px pill with its own radius among 26px
boxes. Three consequences of switching to `Button variant="light" size="compact-sm"`:

- **The bar's existing rules apply.** `--button-height`, `--button-fz: 12px`,
  `font-weight: 400` and the theme's `md` radius all arrive from the
  `.mantine-Button-root` block, so the CSS here is *only* colour now. It also
  means a badge can no longer drift out of step with the bar, since there is
  nothing left to keep in step.
- **`loading` is Mantine's**, so the first-run spinner is centred in place of the
  glyph and the width does not change when a refresh starts — no shuffling of the
  controls beside it.
- **It is the honest element.** These are clickable and carry a context menu, so a
  real `<button>` is what a keyboard and a screen reader should find. A clickable
  `Badge` is a `<div>` with an `onClick`.

The general rule, worth keeping: **a thing in the chrome that responds to a click
is a Button, and reaching for a Badge to get a pill shape means re-implementing a
control.** `leftSection` is how a glyph goes in one, never a hand-spaced `<span>`.

### 2026-08-12 — An action invalidates; it is not rate-limited against

Reported from use as "clicking the demo badge sometimes loads but the colour does
not change". The script was correct and the system was wrong, which is worth
recording because the shape recurs.

Running an action, then forcing a refresh, was answered from the run made
*before* the action — because a forced refresh is bounded by
`FORCED_REFRESH_FLOOR` (3s), and a click plus a re-read lands well inside it. So
the badge showed a spinner and then the old value, intermittently, depending on
how fast the user clicked.

**The fix is a distinction the cache did not make: a repeated question deserves a
rate limit, a state change deserves an invalidation.** An action now forgets the
worktree's remembered values, so the next request must re-run whatever the clock
says. The floor stays for the user's own Refresh, where it does the job it was
written for.

Two details in the implementation that are easy to get wrong the other way:

- **The cells are cleared in place, not removed from the map.** Dropping the entry
  would let the next request mint a fresh cell and start a second child while the
  first was still running — reintroducing the stampede the single-flight design
  exists to prevent, in a narrower window.
- **A *failed* action invalidates nothing.** It changed nothing worth re-reading,
  so the badge keeps saying what it last truthfully said rather than being reset
  by an action that did not happen.

### 2026-08-13 — Extensions are worktree-based, and `ide.news` is not the precedent

The threat-model angle of the review raised this as a critical finding and it went
to the maintainer, because it sets the trust model for the whole system rather than
one feature. **Decision: a worktree's own `veld.json` drives its extensions.**

The finding was that automatic evaluation reads the *checked-out* config, so a
branch someone else authored — a fork pull request checked out for review — has its
badge commands run without a click, and that `ide.news` had already been given the
opposite rule (taken from the **main** checkout, because "a card being drafted in a
worktree must not prompt a teammate").

**Worktree-based is the rule; `ide.news` is the exception that had to earn its way
out.** Getting that direction right matters, because it decides which of the two
needs a justification. Veld's core model — since the first version, before any of
this — is that the checked-out worktree's `veld.json` decides what runs: the
dependency graph, the services, the panes, the actions. Extensions are that same
kind of thing and inherit it by default. News is the one surface that does *not*,
and the reason is worth keeping, because the two differ in kind on two properties
rather than in degree:

- **News is published *to other people*, and it mutates their durable state.** A
  card is delivered to every teammate and recorded per id in their database
  forever — which is exactly why its ids can never be renamed or reused. So "must
  not reach a teammate before it has landed on main" is a rule about *publication*.
- **An extension has no persisted state and no audience.** It renders in the top bar
  of whoever checked that branch out, for as long as they are looking at it, and
  nothing survives. There is nobody else for a draft to reach.

And the positive argument, which is the load-bearing one: **veld's core model is
already that the checked-out worktree's `veld.json` decides what runs** — the
dependency graph, the services, the panes, the actions. An extension being the one
thing resolved from somewhere else would be the anomaly, and it would break the loop
the feature is for: check out a branch, write the extension, *test it*, then merge the
PR (with a news card if the change is worth telling the team about). Main-only
declarations make an extension untestable until after it has shipped.

So the trust boundary stays where the rest of veld already puts it: **checking a
worktree out and opening it is the act of trust.** What that costs is stated plainly
in [Security posture](#security-posture) rather than left implicit, together with the
one lever for somebody who reviews untrusted branches — the machine-global
`extensions.autoRefresh` switch, which stops the unattended half while leaving
clicks working.

### 2026-08-13 — An icon-only badge is a presentational field, `text` stays the name

**Chosen:** `display: "text" | "icon"` on a `status` extension (default `"text"`),
overridable per value the same as `open_in`. `icon` renders the glyph alone as the
whole badge; `text` (or the declared `label`, absent that) is kept as the button's
accessible name and the tooltip's fallback, never dropped. Falls back to `"text"`
when there is no glyph to actually render alone, from either the value or the
declaration, rather than an empty box.

The motivating case is a worktree-staleness badge that wants to *be* a red
`alert-triangle` rather than a pill of text — and specifically not a number: "0
commits behind" reads as clean against a three-week-old `origin/main` with a
history of small commits, so the honest signal is a colour and a shape, not a
count. That case is core data (see the staleness note in the backlog below); only
its *rendering* as a glyph-only badge is this decision.

Rejected, with reasons:

- **Omit `text` and let the label fall away on its own.** This is the bug the
  issue started from, not a design: `crates/veld-daemon/src/extensions.rs`
  already substitutes the declared label when `text` is absent, specifically so
  an author emitting only `{ "href": … }` still gets a visible, clickable badge
  (`an_object_with_no_text_falls_back_to_the_declared_label` pins it). That
  fallback is deliberate and stays; "just omit `text`" would have to delete it to
  work, trading a good invariant for this one's convenience.
- **`"text": null`.** JSON `null` vs. absent is a distinction the lenient `ide`
  parser deliberately makes nowhere else, and it still loses the accessible
  name — a sighted-only fix for what is, underneath, an accessibility field.
- **`"icon_only": true`.** A boolean can only ever mean "yes" or "no" — it cannot
  grow a third rendering later without becoming two fields that can disagree,
  which `display` as an enum does not have to solve for because it already isn't
  one.

### 2026-08-13 — `${veld.branch_raw}`: unslugified, `argv` only, refused in `shell`

**Decision: add `${veld.branch_raw}` — the checkout branch name, unslugified —
to the pane and extension variable scopes, permitted in `argv`, and a `veld
lint` finding when used in `shell`.**

`${veld.branch}` is slugified (`feat/foo` → `feat-foo`), which is documented
across every reference for this scope as a trap: the obvious
`["gh", "pr", "view", "${veld.branch}"]` is not an error, it is a wrong
answer, and every worked adapter sidesteps it with
`git rev-parse --abbrev-ref HEAD` inside the command instead. The slugging
itself stays — `${veld.branch}` also reaches hostnames, and the value is one
an outsider chooses (checking out somebody's pull-request branch), so an
unslugified `${veld.branch}` would be a second, worse footgun. This adds the
raw value under its own name rather than changing what `branch` means.

**The `argv`/`shell` split is measured, not assumed.**
`git check-ref-format --branch` on this machine accepts `foo$(id)`, `foo|bar`
and `foo'bar` as valid branch names, alongside the intended `feat/foo` and
`UPPER.Case`. Two consequences follow, and both are load-bearing:

- **`shell` cannot be made safe by advice.** A raw ref substituted into a
  `shell` string is command execution on checkout of somebody's PR branch, and
  the standard mitigation — quote your interpolations — does not work, because
  `foo'bar` breaks out of a single-quoted substitution too. So `shell` refuses
  it outright rather than warning about it.
- **`argv` closes the shell hole**, because interpolation runs after the
  argv's element count is already fixed, so `$(...)`, `|` and `&&` are inert
  text inside one argument.

**A first draft of this entry additionally claimed "`argv` is safe" outright,
reasoning that git refuses a leading `-`.** That is true of the `git branch`
porcelain but not of the ref grammar: `git switch -- -foo` (or fetching and
checking out a remote branch already carrying that name) succeeds, and `git
worktree list --porcelain` reports it verbatim, with no validation on that
discovery path (`validate_branch` in `crates/veld-daemon/src/desktop.rs` only
guards branch names the daemon *creates*). `["gh", "pr", "view",
"${veld.branch_raw}"]` on such a worktree would hand `gh` a flag, not a ref —
argument injection, not shell injection, but a real hole in a design whose
whole premise is an attacker-chosen branch name. Caught in review before this
shipped, not after. The fix is at the same source `validate_branch` already
treats as the threat: `worktree_builtins` in `crates/veld-daemon/src/pty.rs`
now omits the `branch_raw` key entirely when the branch starts with `-`, so
`${veld.branch_raw}` fails closed with the same "unresolved built-in" error
any other unpopulated variable produces, rather than resolving to something
usable as a flag. `${veld.branch}` never needed this: `url::slugify` never
starts or ends with `-`, which is the one respect in which the slugified and
raw forms are not simply the same value minus formatting.

This holds for extensions unconditionally (`spawn_command` in
`crates/veld-daemon/src/extensions.rs` execs the resolved `argv` directly, no
shell). For panes it holds by a different, and equally deliberate, mechanism:
a pane's `argv` command is still run inside the user's login+interactive shell
(`login_shell_command` in `crates/veld-daemon/src/pty.rs`), but each element is
re-quoted with `veld_core::console::quote` — correct POSIX single-quote
escaping — before being joined into the script handed to `sh -c`, so
`foo'bar` still arrives as one shell word.

**Panes get it too, deliberately, not by the shared builtin map's inertia.**
`worktree_builtins` (`pty.rs`) is the single source both `ide::PANE_BUILTINS`
and `ide::EXTENSION_BUILTINS` are tested against
(`pane_context_populates_every_lintable_name`,
`extension_commands_resolve_exactly_the_names_lint_accepts`), so adding
`branch_raw` there and to only one of the two allowlists would fail a test —
but the reason to add it to both is independent of that test: a pane command
has the identical footgun (`${veld.branch}` reads wrong to a `gh`/`glab` call)
and, per the point above, the `shell`-refusal reasoning applies with *more*
force there, not less, since a pane's `shell` form is always fed to an
interactive login shell.

Rejected:

- **`${veld.ref}`.** Short and it is git's own word for "a name that resolves
  to a commit", but GitHub's own Actions runner had to split `GITHUB_REF`
  (`refs/heads/foo`) from `GITHUB_REF_NAME` (the short name) precisely because
  "ref" alone reads as the former to anyone who has used `git symbolic-ref` or
  `git for-each-ref`. Naming it `ref` here would reintroduce the exact
  ambiguity GitHub's own convention exists to avoid, in a name that — being
  `veld.*` — can never be renamed once someone's config depends on it.
- **`${veld.ref_name}`.** Mirrors `GITHUB_REF_NAME` and avoids the ambiguity
  above, but sorts away from `${veld.branch}` in every allowlist, error
  message and doc table (`branch`, `pane.id`, … vs `branch`, `branch_raw`,
  `pane.id`, …). `branch_raw` lands immediately next to `branch` everywhere the
  trap is already documented, so the escape hatch appears beside the trap with
  no cross-reference needed — including in the `veld lint` error message
  itself, which lists the allowed set. Adopted instead.
- **Not adding it at all**, and documenting `git rev-parse --abbrev-ref HEAD`
  harder instead. That one line is arguably *more* correct — it reads live
  `HEAD`, while veld's `branch` comes from a synced database row and can
  disagree if something else was checked out in that worktree since the last
  sync — and every adapter wanting the ref is already running git or a
  provider CLI with somewhere to put it. Rejected on convenience and
  discoverability grounds: this repo's own adapters, and every doc file listed
  above, already carry the `git rev-parse` idiom as a workaround for a trap
  that a lint-time escape hatch removes at the source. The cost taken on is a
  permanent name in a closed set plus one scope rule (§`check_command_variables`
  in `crates/veld-core/src/ide.rs`) — judged worth it because the alternative
  is a footgun that stays documented as a footgun forever rather than fixed.

### 2026-08-13 — Reversed: extensions default to the main checkout, and it is now a setting on both surfaces

The same day's "Extensions are worktree-based, and `ide.news` is not the
precedent" entry above is **reversed** here, on maintainer instruction, once its
cost showed up in the one scenario that entry's threat-model argument did not
weigh: **onboarding.** A new user's existing worktrees were cloned before the
project's `veld.json` gained `ide.extensions` (or before a given badge was
added to it), so the worktree-based default rendered nothing in them — a user
who saw an "extensions" news card and then went to keep working in an old
checkout found no evidence the feature existed until they created a fresh one.
`ide.news` never had this problem, because it was already resolved from the
main checkout for an unrelated reason (publication to a whole team).

**Chosen:** a new setting on each surface, both defaulting to `main`:

- `extensions.source` (`main` | `worktree`, default `main`) — see
  `ConfigSource` in `crates/veld-core/src/db/settings.rs`. `main` reads
  `ide.extensions` from the repo's main checkout regardless of which worktree
  is being looked at; commands still execute in the worktree on screen, with
  its own `branch`/`root` builtins — only the *declarations* move. `worktree`
  restores the original, still-available behaviour, for testing a new or
  edited declaration in the branch that is writing it, before merging.
- `news.source` (`main` | `worktree`, default `main`) — makes the existing
  main-checkout behaviour a setting rather than a hardcoded rule, for the same
  testing reason. `worktree` unions every checked-out worktree's own
  `ide.news` (deduped by id, still capped at `MAX_NEWS_ITEMS`) rather than
  picking one, because there is no single "current worktree" at the layer
  `repo_view` answers from (one response describes every worktree of a repo,
  for every window's poll) — this is explicitly a testing posture, not a
  production one, and it trades away the "a draft cannot reach a teammate"
  guarantee the `main` default exists to hold.

**The reversal is a net security improvement for the flagship threat, not a
regression of it.** The prior entry's own worked example — reviewing a fork
pull request by checking it out — is exactly the case `main`-by-default now
protects: an untrusted branch's `ide.extensions` no longer run automatically
just because it is checked out, since the declarations consulted are the
project's already-merged ones. What is genuinely given up is the *test-before-merge*
loop that entry was written to protect, and `extensions.source = worktree`
exists to restore it on demand — the same shape `extensions.autoRefresh`
already uses for "off machine-wide, on for a click."

Rejected:

- **Leaving extensions worktree-based and fixing onboarding some other way**
  (e.g. seeding new worktrees with the project's current `ide.extensions` at
  creation time). Rejected because it only fixes *future* worktrees — every
  worktree that already existed before this shipped stays blank forever, which
  is the actual reported case (an existing worktree, not a new one), and it
  reintroduces a second, harder-to-explain rule (worktrees created before date
  X behave differently from ones created after).
- **Making `main` the only behaviour, with no setting.** Rejected because it
  deletes the test-before-merge loop the original entry designed for, with no
  way back — and because `ide.news` already established the pattern of "a
  setting, not a hardcoded rule" being worth the small cost for exactly this
  kind of testing need.
- **One setting governing both surfaces.** Rejected: `extensions.source` and
  `news.source` are independent decisions in practice — someone testing an
  extension does not necessarily want every worktree's draft news card
  reaching them too, and vice versa — and coupling them would be the same
  mistake `terminal.shellIntegration`/`terminal.agentIntegration` were split
  apart to avoid (see that pair's own history above).

**Should this be promoted?** No. `ide.extensions` itself has never had a
promotion card — nobody outside the person who wrote this document's Tier 1
round has been told the capability exists yet, so there is no population of
users who already formed an expectation this reversal could violate. The only
readers who would notice a default flip are project maintainers who have
themselves already written `ide.extensions` in a `veld.json`, in the roughly
one day since it shipped — a small, technically-engaged group who reads
`docs/configuration.md` and `Settings → General`, not the audience a
once-ever interrupt exists to reach.

## The extension backlog

Everything in this table is **customization-layer by the tests above** — none of
it is core Veld today or in the near future. It is the backlog the future system
must be able to serve. **Agents add to this table** (see Rules below) when a
feature request lands in the customization realm; they do not silently drop the
idea.

| Feature | Data contract it needs | UI surface | Status |
|---|---|---|---|
| PR / merge-request status (open, draft, closed, merged) | provider API (`gh`/`glab`/`bb`) | top bar, rail row | **top bar: Tier 1 round 1** |
| Open a worktree in an external IDE (WebStorm, VS Code, …) | a local binary per editor | top bar | **Tier 1 round 1** (`type: "action"`) |
| CI check status for a worktree's branch | provider API | top bar, worktree detail | backlog — expressible as a second `type: "status"` today |
| Per-worktree staleness ("branch is N behind origin") | **already exposed as core data** — see note | rail row, worktree detail | core data shipped; badge = extension |
| Inline file blame / "who touched this" | provider API or tool output | editor surfaces | backlog |
| Custom project health badges (coverage, lint gate) | project commands | rail row | backlog — needs the `rail` slot |
| Per-project setup/teardown on worktree create/delete | lifecycle hooks (Tier 1) | n/a (background) | backlog — home is the reserved `hooks` key |
| Launch a local review tool and open it in a browser pane (e.g. [difit](https://github.com/yoshiko-pg/difit)) | a local binary that serves HTTP on a port | top bar action + browser pane | backlog — needs an action that can *start a server and route a pane at it*, which round 1's fire-and-forget action cannot express |
| Badges on every worktree at once | same as the badges themselves | rail row | backlog — the trigger for moving evaluation to the daemon-owned push model |
| Migrate `ide.quicklinks` into `ide.extensions` as `type: "link"` | none (static) | browser pane place list | backlog — the first planned breaking consolidation |
| An availability predicate for a **GUI application**, not just a `PATH` binary | a filesystem probe (`/Applications/X.app`, a Windows registry key, a `.desktop` file) | every slot | backlog — see the note below |

> **Note on `requires_bin` and GUI apps:** `requires_bin` asks the user's `PATH`,
> which is the right question for a CLI and the *wrong* one for the flagship
> "open this worktree in my editor" case. VS Code's `code`, and JetBrains'
> `webstorm`/`idea`, are shell launchers that are **not installed by default** —
> so a PATH check hides the option on a machine where the editor is sitting in
> `/Applications`. Hiding a working option is worse than offering one that
> explains itself, so this repo's own config drops `requires_bin` on its editor
> entries and falls back to the application bundle inside the command. That works,
> and it is a workaround: the missing thing is a predicate that can ask "is this
> application installed", which is per-platform and therefore a real design
> question rather than a field to bolt on. Until then, prefer *no* predicate over
> a `PATH` one for anything with a GUI.

> **Note on staleness:** the data this badge would render is *core* — the daemon
> computes "main checkout is N commits behind `origin/<default>`" and carries it
> on the repo view, because worktree freshness is core (universal-primitive
> test). Rendering it per-worktree as a coloured badge is extension.

## Rules for agents

When a change request proposes a **new feature** for Veld (a capability, not a
bugfix in an existing one):

1. **Classify it** with the universal-primitive / data-contract tests. State the
   verdict in one sentence in the plan or PR body.
2. **Core verdict** → proceed as normal; the AGENTS.md documentation checklist
   applies.
3. **Customization verdict** → add a row to the [extension backlog](#the-extension-backlog)
   in the same PR (feature, data contract, UI surface, status), and keep the
   *core* part of the request (if any) separate. Do not build the extension
   itself in that PR unless the maintainer explicitly scopes it.
4. **Don't delete backlog rows** without the maintainer — the backlog is the
   record of what the future system must serve; pruning it is a product call.
5. **Append to the [Decision log](#decision-log), never rewrite it.** When work on
   the extension system hits a fork that is expensive to reverse — a config key, a
   wire shape, a security posture — record the choice *and the rejected
   alternatives with the argument that killed each one*. This document is the
   system's memory across sessions; a decision whose alternatives are lost gets
   re-argued from scratch by the next agent.
6. **Choosing `open_in` is a judgement about *whose session* the page belongs
   to, and it is the field an agent writing a config gets wrong.** The rule:
   **anything behind a login the developer holds — a code host, a CI dashboard, a
   cloud console, an error tracker — is `system`**, because a Veld browser pane
   has its own cookie jar and lands them on a sign-in page (or a dead end, for an
   SSO flow that will not run in a fresh partition). **`pane` is for what the run
   itself serves**: localhost, a staging URL behind the same session the app
   already uses, a local report a tool just generated. `system` is the default for
   exactly this reason, so the field is usually only written to say `pane`. When
   in doubt, ask whether the reader is already signed in to it somewhere else; if
   yes, it is `system`.
7. **A new extension `type` or `slot` is cheap; a change to the item vocabulary is
   not.** `id`/`slot`/`type`/`label`/`icon`/`requires_bin`/`when_missing`/`argv` are
   shared across every present and future extension kind, including the lifecycle
   hooks that will live under `hooks`. Adding a field is fine; renaming or
   repurposing one of those breaks every project that adopted it.

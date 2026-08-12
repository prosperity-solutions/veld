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
narrower than the vision: **one slot, two extension types, no lifecycle hooks, no
sandbox.** What it fixes for good is the *vocabulary and the placement* — those
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
        "refreshSeconds": 60,
        "requiresBin": "gh",
        "whenMissing": "hint",
        "hint": {
          "text": "Install the GitHub CLI to see this branch's pull request.",
          "href": "https://cli.github.com"
        }
      },
      {
        "id": "webstorm",
        "slot": "topBar",
        "type": "action",
        "label": "Open in WebStorm",
        "icon": "code",
        "argv": ["webstorm", "${root}"],
        "requiresBin": "webstorm",
        "whenMissing": "hide"
      }
    ]
  }
}
```

Fields common to every type: `id` (stable, safe identifier, unique per project),
`slot`, `type`, `label`, optional `icon` (from the existing pane-icon allowlist),
`requiresBin`, `whenMissing`, `hint`. `argv`/`shell` follow the house rule for
anything that runs something; `${…}` interpolation reuses the **pane variable
context** (`root`, `branch`, `worktree`, `project`, `username`) so extensions do
not introduce a second vocabulary.

Order within a slot is array order. An unknown `slot` or `type` is a non-fatal
`validate` finding and the item is ignored — the same leniency the rest of the
`ide` block already has, so a project can adopt a newer Veld's slot without
breaking on an older one.

### The two types

**`type: "status"`** — a badge. The daemon runs the command in the worktree root
and parses stdout as the badge contract:

```json
{ "text": "PR #283 · merged", "tone": "success", "tooltip": "…", "href": "https://…" }
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

`href` is opened in a browser pane. Only `http`/`https` are accepted, matching the
refusal `ide.quicklinks` already applies to `vscode://` and `file://`.

**`type: "action"`** — a button. Click runs the command; there is no output
contract and no refresh. Failure surfaces as a toast, the established error
surface for run diagnostics.

### Availability, and why it is a teaching surface

`requiresBin` is resolved daemon-side against `cached_user_path()` — the same
check `ide.panes` already does for `requires_bin`. `whenMissing` decides what an
unavailable item looks like:

| `whenMissing` | Behaviour |
|---|---|
| `hide` | The item is not rendered. For optional tooling nobody should be nagged about (a specific editor). |
| `disable` | Rendered greyed, tooltip names the missing binary. |
| `hint` (default) | Rendered greyed with the `hint` text, and its `href` opens the install page. This is the newcomer path: a fresh clone *shows you* what the project expects you to have. |

**An explicit `whenMissing` wins over the global `ui.hideDisabledActions`
setting.** That setting is about hiding inapplicable *core* actions; an extension
author choosing `hint` is teaching, and a user preference about clutter must not
silently delete the lesson.

### How a badge is evaluated

Round 1 is a **stateless, single-flight RPC**, not a daemon-side scheduler:

- The UI asks for the **currently visible worktree only**, over one CSRF-gated
  `POST`, on worktree switch and then on its own interval while the window is
  focused. Nothing is evaluated for the other 17 registered worktrees, and
  nothing is evaluated while no window is open.
- The daemon holds no badge values — only a map of in-flight runs, so three
  windows asking at once collapse to **one child process**, and a request inside
  the minimum interval is refused rather than re-run. There is no TTL cache, and
  therefore no stampede at the moment a TTL expires.
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

- **`stdin` is closed and no tty is attached**, so a CLI that would prompt for
  credentials fails instead of hanging forever.
- **A hard timeout**, enforced by killing the **process group**, not the pid.
- **A byte cap on captured output**, and stdout is rendered as text — never as
  markup.
- **A minimum `refreshSeconds` floor and a maximum extension count per worktree**,
  both named constants, so the cost bound is set by Veld and not by a file in
  somebody's repo (the same reasoning as `PRESETS_EXPANDED_PER_LISTING`).
- **`NO_COLOR=1` / `TERM=dumb`** in the child environment, so a CLI that colours
  its piped output cannot corrupt the contract.
- **Every execution is logged with its full argv** to the daemon log.
- **A machine-global off switch** in settings disables all automatic evaluation —
  the answer for a paranoid machine or a CI box, at zero cost to everyone else.
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
  *item vocabulary* (`type`, `argv`, `requiresBin`, …) is meant to be reused there
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
  rate-limited token on 18 worktrees to serve the one being looked at, forever,
  including overnight with the IDE closed. Its *delivery* half is the right answer
  later; its *trigger* half is not.
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
6. **A new extension `type` or `slot` is cheap; a change to the item vocabulary is
   not.** `id`/`slot`/`type`/`label`/`icon`/`requiresBin`/`whenMissing`/`argv` are
   shared across every present and future extension kind, including the lifecycle
   hooks that will live under `hooks`. Adding a field is fine; renaming or
   repurposing one of those breaks every project that adopted it.

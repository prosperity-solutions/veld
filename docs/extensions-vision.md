# Extensions vision — config-driven customization for the Veld IDE

> This document is the reference point for a config-driven customization and
> extensibility capability for the Veld IDE. It is deliberately written *before*
> that capability exists. Its two jobs: pin down where the line between **core**
> and **customization** is drawn, and hold the **backlog of extension features**
> the future system must be able to serve. Agents and humans both update it — see
> [Rules for agents](#rules-for-agents).

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

## The extension backlog

Everything in this table is **customization-layer by the tests above** — none of
it is core Veld today or in the near future. It is the backlog the future system
must be able to serve. **Agents add to this table** (see Rules below) when a
feature request lands in the customization realm; they do not silently drop the
idea.

| Feature | Data contract it needs | UI surface | Status |
|---|---|---|---|
| PR / merge-request status (open, draft, closed, merged) | provider API (`gh`/`glab`/`bb`) | top bar, rail row | backlog |
| CI check status for a worktree's branch | provider API | top bar, worktree detail | backlog |
| Per-worktree staleness ("branch is N behind origin") | **already exposed as core data** — see note | rail row, worktree detail | core data shipped; badge = extension |
| Inline file blame / "who touched this" | provider API or tool output | editor surfaces | backlog |
| Custom project health badges (coverage, lint gate) | project commands | rail row | backlog |
| Per-project setup/teardown on worktree create/delete | lifecycle hooks (Tier 1) | n/a (background) | backlog |

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

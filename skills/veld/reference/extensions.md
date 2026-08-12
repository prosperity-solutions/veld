# Customizing the Veld IDE: `ide.extensions`

Authoring reference for the badges, buttons and menus a project contributes to the
Veld IDE's top bar. The field table is in [config.md](config.md#ideextensions); this
page is how to **decide what to write**, plus adapters you can copy.

Read [Before you add one](#before-you-add-one) first. The commonest mistake is not a
malformed config, it is adding four badges nobody needed to a 42px bar.

---

## The one-minute model

- A project declares extensions in `veld.json` under `ide.extensions` — **one flat
  array**. `slot` is a *field* on the entry (`topBar` is the only one today), not a
  level of structure.
- `type` says what it is: **`status`** (a badge backed by a command veld re-runs on
  a timer), **`action`** (a button that runs a command on a click), **`menu`** (one
  control whose members are `action` entries).
- A badge's command **prints a small JSON contract on stdout** and veld renders it.
  That is the whole extension mechanism: **veld never learns your code host's
  name.** The provider-specific knowledge lives in your command.
- Everything runs in the worktree root, with the user's login-shell `PATH`.

```jsonc
"ide": {
  "extensions": [
    { "id": "pr", "slot": "topBar", "type": "status", "label": "PR",
      "icon": "git-pull-request",
      "argv": ["scripts/veld/pr-badge.sh"],
      "refresh_seconds": 60,
      "requires_bin": ["gh"],
      "when_missing": "hint",
      "hint": { "text": "Install the GitHub CLI to see this branch's pull request.",
                "href": "https://cli.github.com" } },

    { "id": "open-in", "slot": "topBar", "type": "menu",
      "label": "Open this worktree in", "icon": "external-link",
      "items": ["vscode", "webstorm"] },

    // No `slot`: reachable only from the menu above.
    { "id": "vscode", "type": "action", "label": "VS Code",
      "shell": "command -v code >/dev/null 2>&1 && exec code \"${veld.root}\" || exec open -a \"Visual Studio Code\" \"${veld.root}\"" },
    { "id": "webstorm", "type": "action", "label": "WebStorm",
      "shell": "command -v webstorm >/dev/null 2>&1 && exec webstorm \"${veld.root}\" || exec open -a WebStorm \"${veld.root}\"" }
  ]
}
```

---

## Before you add one

**The bar already holds around sixteen controls.** Every badge competes with the
run controls for the same row, and a badge is *permanently* on screen — unlike a
notification, it never goes away, so a badge nobody reads is worse than no badge:
it trains people to ignore that region.

Add one when **all** of these hold:

1. Somebody looks this up several times a day, in another window.
2. The answer is short enough to read without stopping — a state, a number, a name.
3. It is about **this worktree**. A project-wide fact belongs in a README; a badge
   that says the same thing in every checkout is wasting the row.

Good: this branch's pull request state. CI result for this branch. The deploy tag
currently live. A staging lock ("held by Sam").

Bad: the current git branch (the rail already shows it). A link with no state —
that is `ide.quicklinks`. Anything requiring a sentence to interpret. Test counts
nobody acts on.

**An `action` is cheaper than a badge** — it costs a click's worth of attention and
nothing when idle — and inside a `menu` it costs almost nothing. Prefer actions and
menus when in doubt.

**Ask the user before adding extensions they did not request.** These are committed
to a shared repo and appear in every teammate's IDE.

---

## The badge contract

A `status` command writes **one JSON object to stdout**:

```json
{
  "text": "PR #284 · checks green",
  "tone": "success",
  "icon": "git-pull-request",
  "tooltip": "Longer explanation, shown on hover.",
  "href": "https://github.com/org/repo/pull/284",
  "open_in": "system",
  "actions": [{ "id": "create-pr", "label": "Create a pull request" }]
}
```

Every field is optional. `tone` is `neutral` | `info` | `success` | `warning` |
`danger`. `icon` takes the same allowlist and emoji rule as the declaration's and
**overrides it**, which is how a glyph tracks state.

### Three tolerances you should exploit

| What the command does | What the user sees |
|---|---|
| Prints something that is **not** the contract | Its **first line** becomes the badge text, tone `neutral`. So `argv: ["git","rev-parse","--short","HEAD"]` is a finished badge with no adapter at all. |
| Exits **0 with no output** | **No badge.** This is how one config serves worktrees where the badge does not apply — a detached HEAD, a branch with nothing to report. Use it instead of printing "n/a". |
| Exits **non-zero** | Badge renders in `danger` with your **last stderr line** as its tooltip. So write a real message to stderr and exit 1; do not swallow errors into a green badge. |

### `actions` are ids, never commands

An entry names a **declared `action` extension**. Veld resolves each id against the
on-disk config before offering it, so a runtime value can *choose* among your
declared commands and can never contribute one. An id that resolves to nothing, or
to a `status`/`menu`, is silently dropped.

This is what makes the empty state useful: no pull request yet → print
`{"text":"No PR","actions":[{"id":"create-pr"}]}`, and the badge offers the button
that fixes it.

A bare string works too (`"actions": ["create-pr"]`), taking the declaration's
label. Max 8 per badge.

---

## Decisions, and how to get them right

### `open_in` — whose session does the page belong to?

**The field most often got wrong.** A Veld browser pane has its **own cookie jar**.

| The link points at | Use | Why |
|---|---|---|
| A code host, CI, a cloud console, an error tracker — **anything behind a login the developer holds** | `system` (the default) | A pane lands them on a sign-in page, and an SSO flow started in a fresh partition is a second login at best and a dead end at worst |
| What **the run itself serves** — localhost, a staging URL on the same session the app uses, a report a local tool just wrote | `pane` | It belongs beside the code, and the pane is already the right browser for it |

Since the default covers the first row, in practice you only write this field in
order to say `pane`. **Test:** is the reader already signed in to that site
somewhere else? Then `system`.

Only `http`/`https` are accepted. `vscode://`, `file://` and friends are dropped
(the badge stays) — a repo-controlled string handed to the OS would make a config
file a launcher for whatever is registered.

### `requires_bin` — and when not to use it

It asks the user's login-shell `PATH`, by **name** (never a path). Right for a CLI.

**Never use it for something with a GUI.** `code`, `webstorm` and `idea` are
launchers installed *separately* from the editor — VS Code needs *Shell Command:
Install 'code' command in PATH* — so a `PATH` check hides the option on a machine
where the application is sitting in `/Applications`. Hiding something that would
have worked is the worse failure. Leave the predicate off and let the command find
the app:

```jsonc
"shell": "command -v code >/dev/null 2>&1 && exec code \"${veld.root}\" || exec open -a \"Visual Studio Code\" \"${veld.root}\""
```

`open -a` exits non-zero for an app that is genuinely absent, so a click reports it
instead of doing nothing. Inside a `menu` an entry that might not work costs a line
in a popover rather than space in the bar.

### `when_missing` — `hint` is a teaching surface

| Value | Renders | Use for |
|---|---|---|
| `hint` (default) | Greyed and dashed, your `hint.text` on hover, `hint.href` on click | **The newcomer path.** A fresh clone *tells* somebody what the project expects them to install. Use it for a tool the project genuinely needs. |
| `disable` | Greyed, missing tool named | A tool worth knowing about but with no install page worth linking |
| `hide` | Nothing | Optional tooling nobody should be nagged about — which editor somebody uses |

An explicit value **beats** the user's *hide disabled actions* setting, because a
project's setup instruction must not be silenced by a clutter preference. Do not
abuse that: `hint` for something optional is a nag with no off switch.

### Slots and grouping

- `slot: "topBar"` renders it. `align` picks the side: `start` (default, the
  project's cluster) or `end` (reads as app chrome).
- **Omit `slot` on an `action`** to declare one reachable only by reference. A
  `status` or `menu` without a slot is a lint error — nothing can reference them.
- **Group at three.** One or two buttons are fine loose; beyond that use a `menu`,
  whose `items` are ids in popover order. A menu is one control in the bar.
- Nesting is one level: a menu's items are `action`s, never menus.

### Variables

`${veld.root}` (worktree path), `${veld.branch}`, `${veld.worktree}` (slug),
`${veld.project}`, `${veld.username}`. Anything else — including `${veld.port}`,
`${output.*}` and the `pane.*` family — is a **`veld lint` problem**, not a runtime
failure, so `veld lint` is the check that a command will resolve.

**`${veld.branch}` is slugified — it is not a git ref.** `feat/foo` arrives as
`feat-foo`. This is the trap to avoid: `gh pr view "${veld.branch}"` is not an
error, it is a *wrong answer* — on any branch with a `/`, a `.` or a capital it
reports no pull request and offers to create a second one. Read the real ref inside
the command with `git rev-parse --abbrev-ref HEAD`. (The slugging is deliberate: the
branch name is chosen by whoever opened the pull request you checked out, so a
`shell` command interpolating it raw would be running their string.)

**Quote interpolations in a `shell` command.** `${veld.root}` can contain spaces.
Prefer `argv`, which cannot change its argument count.

---

## Writing the adapter script

Keep it in the repo (`scripts/veld/…`) and keep it small. It is the only place that
knows your provider.

**Rules that matter:**

- **It is re-run from scratch every time.** There is no state between runs. Anything
  it must remember goes on disk, in a gitignored directory.
- **A badge cannot advance itself.** Clicking a badge opens its `href` or runs an
  `action`; it does not re-run the badge's own command. If a click should change
  what the badge says, that click is an `action`, and veld re-reads the badges as
  soon as one has run.
- **`stdin` is closed and there is no tty.** A CLI that would prompt for
  credentials fails instead of hanging — which is what you want, but it means you
  must capture stderr and pass it on, or the user gets a red badge with no reason.
- **One API call.** Fetch once, derive every field from that payload. The badge runs
  on a timer against a rate-limited token.
- **20 seconds, then the process group is killed.** Do not retry inside the script.

### Worked adapter: GitHub pull request

`scripts/veld/pr-badge.sh` in this repo is the maintained version; the shape:

```bash
#!/usr/bin/env bash
set -uo pipefail

branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null) || branch=""
# No branch to have a PR for. Not an error — nothing to show.
[ -z "$branch" ] || [ "$branch" = "HEAD" ] && exit 0

if ! payload=$(gh pr view "$branch" --json number,state,isDraft,url,statusCheckRollup 2>&1); then
  case "$payload" in
    # The interesting empty state: no PR *yet*. Offer the action that fixes it.
    *"no pull requests found"*)
      printf '{"text":"No PR","tone":"neutral","actions":[{"id":"create-pr"}]}\n'; exit 0 ;;
    # Anything else is a real failure: stderr becomes the badge's tooltip.
    *) echo "$payload" >&2; exit 1 ;;
  esac
fi

# …derive text/tone/href from $payload and print one JSON object.
```

**For GitLab**, the declaration is unchanged — swap `gh` for `glab` inside the
script. That is the point of the contract.

### Worked adapter: anything already printing one line

No script needed at all:

```jsonc
{ "id": "sha", "slot": "topBar", "type": "status", "label": "sha",
  "argv": ["git", "rev-parse", "--short", "HEAD"], "refresh_seconds": 60 }

{ "id": "coverage", "slot": "topBar", "type": "status", "label": "cov",
  "shell": "cat target/coverage/summary.txt 2>/dev/null", "refresh_seconds": 300 }
```

The second one is worth studying: `2>/dev/null` plus a missing file gives exit
1 — a red badge. Add `|| exit 0` if "no coverage yet" should simply show nothing.

---

## How veld runs these (and the bounds you cannot change)

Not tunable from config. Know them, because they explain behaviour that otherwise
looks like a bug:

- **Only the worktree on screen is evaluated**, and only while a window is open.
  Registered worktrees nobody is looking at cost nothing.
- **Several windows share one child process.** A request inside an extension's
  `refresh_seconds` is answered from that run, with its age reported.
- **`refresh_seconds` is floored at 15**, defaults to 60. **Max 24 extensions per
  project.** Both are veld's bounds, not the repo's.
- **20s deadline**, enforced by killing the process group. **Output is capped.**
  `NO_COLOR=1` and `TERM=dumb` are set, so a tool cannot put escape sequences in
  the contract.
- **Every command is logged with its full argv** to the daemon log.
- **Right-click a badge** to re-run it, or all of them. A forced refresh ignores
  `refresh_seconds` (3s floor instead) and reports its own errors — the background
  poll is deliberately silent.
- **Running an `action` re-reads the badges immediately**, because an action usually
  changes what one of them says.
- The user's **`extensions.autoRefresh`** setting (default on) turns the unattended
  half off machine-wide. Actions and menus keep working — a click is the user
  asking. **There is no consent prompt**; the reasoning is in
  `docs/extensions-vision.md`.

---

## Checking your work

```sh
veld lint                      # unknown key, bad reference, bad variable, clamped interval
./scripts/veld/pr-badge.sh     # run the command yourself; is stdout one JSON object?
echo $?                        # 0 with output = badge, 0 with none = hidden, non-zero = red
```

`veld lint` is the only check that a declaration **took**. Everything under `ide` is
lenient by design — a malformed entry is a warning and a dropped entry, never a
load error — so a silent typo shows up there and nowhere else.

Two failure modes worth reproducing on purpose before you commit:

1. **Rename the required binary** (or unset it from `PATH`) and confirm the
   `when_missing` rendering says something useful.
2. **Make the script exit 1** and confirm the tooltip carries a message a colleague
   could act on.

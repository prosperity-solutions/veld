# Showing the human a file (`veld ide open`)

**A capability worth knowing you have.** When you run in a Veld terminal, the
human is looking at a Veld window, and you can put a file or a page in it — in a
tab beside your terminal — instead of printing a path and hoping they go and look.
A plan you wrote, a CSV you produced, the function you want them to read, the
report you rendered, the dev server's page you just changed.

```sh
veld ide open plans/rollout.md --notify      # stop and read this
veld ide open src/api.ts:120 --quiet     # here it is, when you get to it
veld ide open http://localhost:5173 --quiet
veld ide open out/report.csv --quiet --json
```

**Use it for deliverables, not for every file you touch.** Each call opens a tab
in somebody's window. Open the thing you want them to read or decide on; do not
open the eleven files you edited on the way there — they have a *Changed files*
list for that (below).

## `--notify` or `--quiet` — you must say which

One of the two is **required**; with neither the command exits 2 and prints
`veld: say --notify (ping the human) or --quiet (just open the tab)`. There is no
default because the two are different requests, and only you know which this is.

- **`--quiet`** opens the tab. Nothing else. Right for "here is the file, for
  reference", or anything they will see because they are already watching.
- **`--notify`** opens the tab **and** asks for attention: an unread mark on the
  worktree in the rail (a book glyph, cleared by looking at the tab), a toast with
  a *Show* button, and an OS notification when the window is not focused. Right for
  "I need you to read this before I go on" — a plan to approve, a question
  answered in a document. Do not use it for progress. The toast and the OS
  notification follow the human's *A coding agent is waiting for you* switch
  (`activity.notifyAgentWaiting`, on by default) and Focus mode; with that off,
  `--notify` still leaves the unread mark but pops nothing.

## What opens where

| Target | Opens in |
|---|---|
| Markdown, CSV/TSV, source code, JSON/YAML/TOML, SQL, logs, other text | a **file pane** |
| HTML, PDF, images | a **browser pane** |
| an `http://` or `https://` URL | a **browser pane** |

`path:line` scrolls to and highlights that line (`src/api.ts:120`); a compiler's
`path:line:col` and a reference's `path:12-18` open at the line too. A relative
path is resolved against **your** working directory, not the worktree root. A file
whose name really ends in `:12` wins over the line reading.

Opening the same path again in the same worktree **re-uses the tab** and moves it
to the new line; it does not stack duplicates. It never switches which worktree
the window is showing, and never steals focus across worktrees.

## Where it opens, and when it refuses

Run from a Veld terminal, it opens **beside that terminal** (it reads
`VELD_PTY_SESSION`). Anywhere else, it uses the worktree that contains the current
directory. It **never falls back to the system opener** — that is the difference
from `veld ide open-url`, which stands in for `open` and must fall through. So a
non-zero exit means nothing was shown, and the message says why:

| Exit | Meaning |
|---|---|
| 0 | opened (or an existing tab activated) |
| 1 | failed: no such file, a kind Veld cannot show, the daemon is not running |
| 2 | usage: neither `--notify` nor `--quiet` |
| 3 | not in a Veld terminal, and the cwd is not inside a worktree Veld knows |
| 4 | no Veld window is showing that worktree |

Exit 1 also means Veld refused the file itself — binary, over 5 MB, a secret or
credential, or a kind it cannot show (see *Limits* below). Retrying will not
change that; say so in your reply instead. With `--json`, a failure prints
nothing on stdout; the message goes to stderr.

On 4, tell the human in your reply instead — they are not looking at that
worktree, so a tab there would go unseen anyway.

`--json` prints `{"opened":"file"|"browser","worktree":…,"path"|"url":…}` on
stdout; the human-readable line always goes to stderr.

## What the human sees in a file pane

Read-only — **nothing in Veld writes files**, so this is a way to show, never to
edit.

- **Markdown** is rendered, with a *Rendered | Source* toggle. Raw HTML in the
  markdown is not rendered and remote images are not loaded (their alt text shows),
  so write the document to stand on its own. A relative link to another `.md`
  opens in the file pane too.
- **CSV/TSV** is a sortable table, every row, however long.
- **Code and other text** has line numbers and syntax highlighting, and — for a
  file git knows — markers in the gutter for lines added, modified and deleted
  since the branch left the default branch. They can switch on **Show deletions**
  (`files.showDeletions`) to see the deleted lines as red rows of a diff instead,
  or **Side by side** (`files.splitDiff`) to give the old text a column of
  its own; line numbers in a reference still refer to the file as it is now.
  **Wrap long lines** (`files.wrapLines`) wraps instead of scrolling sideways.
  All three are switches in the pane's header and settings that stick.
- **It reloads itself** while visible when the file changes, keeping the scroll
  position. Rewrite the file and they see the new version; you do not need to open
  it again.

Limits: about 5 MB; binary files are refused with a message in the pane;
secrets (`.env*`, keys, `.ssh`, and their neighbours) are always refused.

## What comes back: references

The human can select lines (or a rendered markdown block) and **copy a
reference**, which they will paste to you as:

```
plans/rollout.md:12-18
> the first quoted line
> the second quoted line
```

The path is worktree-relative when the file is inside the worktree and absolute
otherwise; a single line is `plans/rollout.md:12`. The quoted lines are the excerpt as
it was when they copied it — if you have edited the file since, find the text, do
not trust the line numbers.

**Copy file contents**, beside it in the header, copies the whole file's text with
no path or quoting — what you get when they paste a file rather than point at
part of one.

## Files outside the worktree

A file pane can show:

- anything inside the worktree;
- anything inside a folder the user listed in **`files.extraFolders`** (*Settings
  → Browser panes → Local files*), e.g. a notes folder;
- an exact file the user opened from the UI, or that **`veld ide open` named** —
  naming an absolute path is what grants it, so you can open
  `~/notes/today.md` without the user configuring anything. Grants are per
  worktree and remembered.

Secrets are refused everywhere, outside the worktree included — and outside it,
so are the places CLIs keep logins (`~/.ssh`, `~/.aws`, `~/.config/gh`, `~/.docker`,
`~/.codex`, `.netrc`, an `auth.json`, …). Of `~/.claude` only Markdown under
`plans/` and `projects/` opens, which is where Claude Code's plans and memories are.

## The other ways a file reaches a file pane

You do not need to know these to use the command, but they explain what the
human may already have open:

- **Clicking a path in terminal output** offers *View in Veld* first. If the
  project declares editor actions (`accepts: "file"` — see `veld agent skills
  ide-extensions`), they are listed under it, and the user's first choice is
  remembered (`terminal.fileAction`, where `"veld"` means the file pane).
- **`open notes.md`** in a Veld terminal, for a text file inside the worktree,
  opens the file pane quietly — **only** when the user has Veld showing that
  kind: *Plain text* on (`files.viewPlainText`, off by default) or a
  `files.viewPatterns` entry that selects it. Otherwise, and for anything outside
  the worktree, `open` hands the file to the system opener as it always did. Use
  `veld ide open` when you want it shown; do not rely on `open`.
- **Changed files** — one click from a new pane — lists every file changed on this
  branch (against the merge-base with the default branch) plus uncommitted and
  untracked ones. This is the human's cheap review surface; it is why you do not
  need to open every file you edited.

## Naming the worktree you are in (`veld ide worktree name`)

The rail lists every worktree by its title, and that title is how the human
tells four agents apart. **Name yours early** — as soon as you know what the task
is, usually in your first reply:

```sh
veld ide worktree name "Fix login timeout"
veld ide worktree name "Review PR 412" --json   # {"id", "path", "name", "previous"}
```

- A few words in sentence case saying what the work is, at most 18 columns (a
  wide character such as 漢 or an emoji counts as two). Not the branch name, not
  a sentence. A blank or longer title is refused with exit `2`.
- **Rename it at most once more, and only early**: when what you learn next —
  what a PR, issue or link is actually about — shows the first name was off.
  After that, leave it alone unless the human asks; they may have renamed it
  themselves, and a title that keeps changing is no landmark.
- **In Claude Code this is enforced once**, when *Notice when a coding agent is
  waiting for you* is on as well. Veld adds a reminder to the first
  prompt of a new conversation, and refuses that turn's stop once if the worktree
  is still unnamed (except on Veld's Claude mod where Claude Code's built-in
  policy mod is seated, typical on a Team plan: there a stop cannot be refused). Naming it first means you never see either.
  A `/clear` in the same terminal is not nudged again; a new `claude` launch is.
- It retitles the worktree containing your working directory (or, when that
  cannot be read, the one your terminal runs in). Only the label changes — never
  the branch, the directory, or the run's hostname.

Exit status: `0` named; `1` failed (the daemon is not running, or refused);
`2` not a usable title; `3` not inside a worktree Veld knows.

## Handing a task to the human in a new worktree (`veld ide worktree new`)

When your reply points out something outside the current task that somebody
would have to fix later — a bug you noticed on the way, an outdated or missing
doc, a refactor the change made obvious, the second half of a plan — do not
quietly do it, do not leave it as a note or an issue for somebody to pick up
later, and do not start a sub-agent in a checkout nobody can see. **Finish that
reply with one short question asking whether to hand it off** as a worktree of
its own — even when they told you to leave it alone, because leaving it is their
call. Most people do not know Veld can do this, so the offer is worth making —
once per piece of work, not on every turn. When they agree, or ask for it
themselves:

```sh
veld ide worktree new --branch fix/login-timeout --name "Fix login timeout" --prompt-file /tmp/task.md
veld ide worktree new --branch docs/api --name "API docs" --prompt "Document the v2 endpoints in docs/api.md"
veld ide worktree groups                       # the groups you can file into
veld ide worktree new --branch spike/cache --name "Cache spike" --group Experiments --from-here --prompt-file - < plan.md --json
```

Veld creates the worktree in **the project your working directory is in** — run
it from the checkout you mean; a directory outside any project Veld knows is
refused (exit `3`), not guessed at. With a prompt it lands in the rail's
**Waiting for you** section, marked as not started yet. The first time the human
opens it, an agent pane starts there with your prompt as its first message, and
from then on it is theirs. **Nothing comes back to you** — no status, no result.
Write the prompt for an agent that has none of your context: what to do, why,
which files, how to tell it is done.

- `--branch` is the new branch, and must not exist yet (exit `1` if it does). It
  is cut where a new branch normally starts (`git.createFrom`), or from this
  checkout's `HEAD` with `--from-here` — which brings your commits, not your
  uncommitted changes.
- `--prompt` or `--prompt-file` (`-` for stdin) is the agent's first message. A
  long or structured one belongs in a file; the limit is 64 KiB. Without a prompt
  you get a plain worktree, filed with the user's other new worktrees, and no
  agent.
- By default the human sees your prompt before anything runs, and can edit,
  start, or move it to the trash; if they have turned that review off
  (`worktree.reviewHandoffs`), it is sent **as you wrote it** the moment they
  open the worktree. Write it for both: an instruction a person can judge at a
  glance, and one that is safe to run unread.
- `--group` files it into one of the human's existing groups instead of
  **Waiting for you** (`--lane` is the same flag). `veld ide worktree groups` lists
  them (`--json` for `{"project", "groups"}`); a name that is not one of them is
  refused before anything is created (exit `4`). With a prompt it still waits
  there, marked, until they open it.
- `--agent` names the pane to start (an `ide.panes` id). Leave it out and the
  human's usual agent for this project starts. An agent this project does not
  declare is refused before anything is created (exit `5`); if the new checkout
  turns out not to declare it (a branch cut from `origin` without it), the
  human's usual agent starts instead and they are told.
- `--outfit` names the outfit profile the new worktree wears — see the next
  section. In a project with outfit profiles it is usually **required**; without
  outfit it does nothing and you can leave it out.
- `--name` is the title the rail shows, and it is **required**: a few words
  saying what the task is, in sentence case — "Fix login timeout", "API docs",
  "Cache spike" — not the branch name and not a sentence. It must fit the
  sidebar: at most 18 columns (a wide character such as 漢 or an emoji counts as
  two). A missing or longer title is refused before anything is created (exit
  `2`).

Exit status: `0` created; `1` refused, so nothing was created; `2` bad flags, a
missing or too-long title, or an unreadable prompt file; `3` not inside a worktree Veld knows; `4` no such
group; `5` no such agent pane; **`6` it may exist without everything you
asked for** — a large checkout can take longer than the command waits, or the
hand-off failed partway — so check the rail and **do not run it again** (the
retry would collide with its branch); `7` an outfit profile is needed and none
was named, or the one named is not offered here (nothing created); `8` the
worktree was created but outfit could not put it in that profile, so no agent
waits in it — the message says how to fix it. `--json` prints `{"id", "path",
"branch", "alias", "name", "waiting", "agent", "outfit"}` on stdout, where
`waiting` says an agent is waiting to start, `agent` is the pane you named or
`null` for the human's usual one, and `outfit` is the profile it wears or `null`
when it follows the clone's.

Hand off one task per worktree, and only when the human would want it as its own
branch. Each one is a checkout on their disk and a row in their rail until they
deal with it.

## outfit profiles (`--outfit`)

[outfit](https://outfit.oss.life.li) gives one person their own copies of a few
tracked files — their `AGENTS.md`, their `.claude/skills` — as **profiles**,
without changing anything for the team. A worktree wears one profile; `base` is
the team's files. Veld takes part only where outfit is **in play**: the `outfit`
binary installed and at least one profile besides `base` (committed in
`.outfit/profiles/`, or local to this clone). Everywhere else, none of this
applies and nothing changes. For everything else about outfit, use outfit itself
— `outfit ls`, `outfit status`, `outfit use`, and `outfit skills agents`.

```sh
outfit ls                                     # the profiles, and what each swaps
veld ide worktree new --branch fix/x --name "Fix X" --outfit lean --prompt-file /tmp/task.md
outfit use lean --here && veld agent restart  # switch this worktree, then restart you to read it
```

- **Handing off.** Where outfit is in play, `veld ide worktree new` **must** be
  told `--outfit <profile>` (exit `7` without one, listing each profile and what
  it swaps). `base` is a valid choice. Pick the one the task calls for; if
  nothing tells you, **ask the human** rather than guess — the profile decides
  which instructions the next agent reads. The project can relax this
  (`"ide": { "outfit": { "required": false, "default": "<profile>" } }` in
  `veld.json`), and so can the person (`outfit.required` in Settings → Git:
  *as the project says*, *always*, *never*); relaxed, a create without
  `--outfit` gets the project's `default`, or follows the clone's profile. A
  repository where `outfit init` was never run is set up by the first pick.
- **Switching where you are.** `outfit use <profile> --here` switches the
  worktree you are in (without `--here` it switches the whole clone). If outfit
  answers with the fix `outfit init`, run plain `outfit init` — never with
  `--agents` or `--team`, which write files other people share — and retry. You read
  your instructions when you started, so the swapped files are not in your head
  yet: run `veld agent restart` next, as the last thing in your turn (below).
- **Editing a swapped file edits the profile**, not the team's copy. `outfit
  status --json` lists the swapped paths.

## Restarting yourself (`veld agent restart`)

When your instructions changed under you — a new outfit profile, an edited
`CLAUDE.md`, a skill installed — run `veld agent restart` (the same as `veld ide
pane restart`). **Make it the last thing you do this turn**: say in your reply
what you will pick up afterwards, and stop. Once your turn ends, the Veld window
restarts your pane with its `resume` command, so the conversation carries on, and
your first message says you were restarted; do not restart again. Work you start
after the command can be cut off by the restart. `--fresh` starts a new
conversation instead; `--prompt` sets that first message.

Only a pane from the project's `ide.panes` can be restarted, and only one that
declares `resume` can keep its conversation. Exit status: `0` the restart is on
its way; `3` not in a Veld terminal; `4` no Veld window is showing this worktree
(tell the human instead); `5` not such a pane, or one that cannot resume (use
`--fresh` only if losing the conversation is fine).

## Why you may already know this

Veld tells Claude Code, Codex CLI and Pi about this command when they start in a
Veld terminal — a few lines appended to the system prompt (Claude's
`--append-system-prompt`, Codex's `-c developer_instructions`, a Pi extension), or,
for Claude on Veld's mod (`terminal.claudeIntegration`), the same lines attached to
the first prompt of each conversation.
The user can turn that off (`terminal.agentContext`, *Settings → Activity*); the
command works either way. For Claude Code in an agent pane from `ide.panes`, in a
worktree where outfit is in play, the first prompt of a new conversation also
carries a short note about its profiles; Codex CLI, Pi and a `claude` typed into a
plain terminal do not get that note.

**If those lines name a full path to `veld`, run that path, not the bare word.**
It means this terminal belongs to a development build of Veld, and the `veld` on
your `PATH` is a different install talking to a different daemon — one that may
not have this command at all. The examples on this page say `veld` for the usual,
installed case.

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
from `veld open-url`, which stands in for `open` and must fall through. So a
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
  project declares editor actions (`accepts: "file"` — see `veld skills
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

## Handing a task to the human in a new worktree (`veld worktree new`)

When you come across work that is separate from the task at hand and deserves
its own branch — a bug you noticed on the way, a refactor the change made
obvious, the second half of a plan — do not quietly do it, do not leave it as a
note or an issue for somebody to pick up later, and do not start a sub-agent in a
checkout nobody can see. **Offer to hand it off**: say what the work is and that
you can put it in a worktree of its own for them. Most people do not know Veld
can do this, so the offer is worth making — once per piece of work, not on every
turn. When they agree, or ask for it themselves:

```sh
veld worktree new --branch fix/login-timeout --prompt-file /tmp/task.md
veld worktree new --branch docs/api --name "API docs" --prompt "Document the v2 endpoints in docs/api.md"
veld worktree groups                       # the groups you can file into
veld worktree new --branch spike/cache --group Experiments --from-here --prompt-file - < plan.md --json
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
- The prompt is sent **as you wrote it**, the moment the human opens the
  worktree. Write it the way you would want an instruction you did not review
  to read.
- `--group` files it into one of the human's existing groups instead of
  **Waiting for you** (`--lane` is the same flag). `veld worktree groups` lists
  them (`--json` for `{"project", "groups"}`); a name that is not one of them is
  refused before anything is created (exit `4`). It still waits there, marked,
  until they open it.
- `--agent` names the pane to start (an `ide.panes` id). Leave it out and the
  human's usual agent for this project starts. An agent this project does not
  declare is refused before anything is created (exit `5`); if the new checkout
  turns out not to declare it (a branch cut from `origin` without it), the
  human's usual agent starts instead and they are told.
- `--name` is what the rail shows. Leave it out and the project's
  `ide.worktreeName` names it from the prompt, if it declares one, else the
  branch does.

Exit status: `0` created; `1` failed and nothing was created; `2` bad flags or
an unreadable prompt file; `3` not inside a worktree Veld knows; `4` no such
group; `5` no such agent pane; **`6` no answer yet** — a large checkout can take
longer than the command waits, and the worktree is most likely still being
made, so check the rail and **do not run it again** (the retry would collide
with its branch). `--json` prints `{"id", "path", "branch", "alias", "name",
"waiting", "agent"}` on stdout, where `waiting` says an agent is waiting to start
and `agent` is the pane you named or `null` for the human's usual one.

Hand off one task per worktree, and only when the human would want it as its own
branch. Each one is a checkout on their disk and a row in their rail until they
deal with it.

## Why you may already know this

Veld tells Claude Code, Codex CLI and Pi about this command when they start in a
Veld terminal — a few lines appended to the system prompt (Claude's
`--append-system-prompt`, Codex's `-c developer_instructions`, a Pi extension).
The user can turn that off (`terminal.agentContext`, *Settings → Activity*); the
command works either way.

**If those lines name a full path to `veld`, run that path, not the bare word.**
It means this terminal belongs to a development build of Veld, and the `veld` on
your `PATH` is a different install talking to a different daemon — one that may
not have this command at all. The examples on this page say `veld` for the usual,
installed case.

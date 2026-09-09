# Resuming earlier sessions: `ide.panes[].sessions`

Authoring reference for the picker that lets a pane reopen a coding-agent session
it never started. The field table is in
[config.md](config.md#idepanes); this page is how to **decide whether to write one
and what to put in it**, plus adapters you can copy.

Read [Before you write one](#before-you-write-one) first. The commonest mistake
here is not a malformed config — it is writing a script for a tool that already
has a picker of its own.

---

## The one-minute model

- A pane's `resume` reopens **the session that pane started**, matched by a token
  veld minted. That is nothing at all the first time you open the pane in a
  worktree, and nothing for any conversation from before the pane existed.
- `sessions` is the other half: **a command that lists what the tool already has
  on disk**, so one can be picked.
- Clicking the pane then **asks which session**, with *Start fresh* as the first
  row. Pick a row, and **the pane's own `resume` command runs** with
  `${veld.pane.token}` set to the value you picked.
- So the script chooses **which session a declared command opens**, and can never
  contribute a command. veld learns nothing about where your tool keeps
  transcripts — that knowledge lives in your script, exactly as a badge's
  provider knowledge lives in its adapter.

```jsonc
"ide": {
  "panes": [
    {
      "id": "claude",
      "type": "terminal",
      "label": "Claude",
      "icon": "sparkles",
      "requires_bin": ["claude"],
      "argv": ["claude", "--session-id", "${veld.pane.token}"],
      "resume": { "argv": ["claude", "--resume", "${veld.pane.token}"] },
      "sessions": {
        "label": "Resume an earlier Claude session",
        "argv": ["scripts/veld/claude-sessions.sh"]
      }
    }
  ]
}
```

**Two requirements the parser enforces**, both of them `veld lint` problems that
drop the picker rather than rendering something that cannot work:

1. The pane must declare `resume`. That is what a pick runs.
2. That `resume` must reference `${veld.pane.token}`. It is how the pick gets
   there. A `resume` like `codex resume --last` ignores it, so every pick would
   open the same session — the one outcome a picker must not have.

---

## Before you write one

**Check whether the tool already has a picker.** `claude --resume` with no
argument opens Claude Code's own list, with timestamps and summaries. `codex
resume` does the same. Those lists are written by the tool, so they cannot drift
from it, and a plain pane running one is less for you to maintain:

```jsonc
{ "id": "claude-pick", "type": "terminal", "label": "Claude (pick a session)",
  "requires_bin": ["claude"], "argv": ["claude", "--resume"] }
```

Write a `sessions` script when at least one of these holds:

- **The tool has no picker of its own.** Most don't.
- **You want the option absent, not empty.** A tool's own picker still opens, and
  still has to say "no sessions" once you are inside it. A `sessions` script that
  prints nothing means the pane opens as it always did, with no dialog at all —
  which is what a fresh clone should feel like.
- **The pick has to land in a pane whose flags you already chose.** This is the
  strong one. A tool's own picker gives you the tool's own defaults; adopting into
  a veld pane runs *your* `resume`, so a session picked into a pane labelled
  *Claude Opus, auto mode* comes back on Opus in auto mode. The label stays true.
- **You want a better list than the tool's** — filtered to this worktree, or
  labelled from your own metadata.

**Do not** add one to a pane whose `resume` is a `--last`-style guess. It cannot
work, and `veld lint` will tell you so.

---

## The stdout contract

**One row per line. Tab-separated, and the tabs are optional.**

```text
<value>
<value>\t<label>
<value>\t<label>\t<detail>
```

- **`value`** is what veld substitutes into `${veld.pane.token}`. Usually the
  tool's session id.
- **`label`** is the row's text. Defaults to the value, so a bare list of ids is
  already a working picker.
- **`detail`** is a second, quieter line. Good for the id when the label is prose.

So the zero-effort case really is zero effort:

```jsonc
"sessions": {
  "shell": "ls -t ~/.mytool/sessions/*.json 2>/dev/null | head -15 | xargs -n1 basename -s .json"
}
```

and the rich case is one `printf`:

```text
1f2e3d4c-8a91-4c02-9f13-77bbd2e5a410	2h ago · fixing the pane resume bug	41 messages
9a8b7c6d-2231-4ff8-b0aa-1e3c9d5f2a77	yesterday · badge tolerance tests	7 messages
```

### Three tolerances you should exploit

These are the same three a `status` badge has, so if you have written one of those
you already know this. They are not error handling; they are the interface.

| Your command… | What veld does |
|---|---|
| exits **0 with no output** | There are none here. **No dialog, no picker, nothing** — the pane opens exactly as it did before you added this. Use this for "not applicable in this worktree" rather than printing a placeholder. |
| exits **non-zero** | The dialog still opens (so the pane is never blocked), with *Start fresh* and the **last line of your stderr**. Under `ask_first: false` there is no dialog, so the card's button is disabled and carries the message instead. Write a real message to stderr rather than swallowing errors — it is the only place your users will see it. |
| prints a **row veld cannot use** | That row is dropped and the dialog says how many. One malformed line never costs you the other nineteen. |

### The value has a charset, and it is a security boundary

A usable `value` is **1–128 characters** of letters, digits, `.`, `_`, `-`, `:`,
`@` or `/`, **starting with a letter or digit**. Anything else is a dropped row.

That is deliberately narrower than "what a session id looks like", because the
value is interpolated into a command:

- The set contains **no shell metacharacter**, so a `resume` declared with `shell`
  is safe with no special case. A row printing `$(id)` is dropped, not run.
- It **cannot start with `-`**, so it cannot be read as a flag by whatever
  receives it. A row printing `--dangerously-skip-permissions` is dropped.

If your tool's ids do not fit that set, the fix is in your script: emit a handle
that does, and translate it back in `resume`.

### Ordering and count

**Ordering is yours** — veld renders rows in the order you print them and never
re-sorts. Newest first is almost always right. veld shows **at most 50** rows and
says when it dropped some, but that is a backstop: a picker of 50 conversations is
a directory listing. Cap it yourself at 10–20.

---

## `ask_first` — the click asks, by default

```jsonc
"sessions": { "argv": ["..."], "ask_first": true }   // the default
```

**`true` (default):** clicking the pane opens the dialog. *Start fresh* is its
first row, so the click the user made is one more click away and always in the
same place.

This default exists because a picker behind a small control is a picker most
people never find — and *"you already have a conversation about this worktree"* is
exactly what somebody needs told **before** they start a second one. The cost is
asymmetric: an unwanted dialog costs one click, a missed picker costs a whole
duplicate session.

**`false`:** clicking the pane launches it, and the list moves into the card's
other half — a labelled button sharing the card's border. Choose this when your
panes are usually a genuine fresh start and resuming is the exception.

Either way, **veld only asks when there is something to ask about**: an empty
answer produces no dialog and no button.

---

## Writing the adapter script

Same shape as a badge adapter, and the same rules apply — `set -uo pipefail`
(not `-e`, which turns a "nothing found" into a non-zero exit), fail soft, and
write real messages to stderr. veld runs it from the worktree root.

**Prefer `find`/`stat` over `ls -t`.** With no matches, `ls` writes to stderr and
exits non-zero, which the contract reads as *"this script is broken"* rather than
*"there is nothing here"* — the difference between a red dialog and a pane that
opens normally.

### Worked adapter: Claude Code

Claude Code has no "list my sessions" command, so this reads the transcripts off
disk. The directory is the working directory with **every non-alphanumeric
character replaced by `-`**.

```bash
#!/usr/bin/env bash
set -uo pipefail

slug=$(printf '%s' "$PWD" | sed 's/[^A-Za-z0-9]/-/g')
dir="${CLAUDE_CONFIG_DIR:-$HOME/.claude}/projects/$slug"
[ -d "$dir" ] || exit 0          # no sessions here — not an error

mtime() { stat -f '%m' "$1" 2>/dev/null || stat -c '%Y' "$1" 2>/dev/null || echo 0; }

rows=$(
  for f in "$dir"/*.jsonl; do
    [ -f "$f" ] || continue
    printf '%s\t%s\n' "$(mtime "$f")" "$f"
  done | sort -rn | head -n 15
)
[ -n "$rows" ] || exit 0

printf '%s\n' "$rows" | while IFS=$'\t' read -r ts f; do
  id=$(basename "$f" .jsonl)
  age=$(( ($(date +%s) - ts) / 3600 ))
  printf '%s\t%sh ago\t%s\n' "$id" "$age" "$id"
done
```

That is a working picker. This repo's own
`scripts/veld/claude-sessions.sh` is the same script plus one extra step: it digs
the first user message out of the JSONL for the row's label, which is what turns
*"3h ago"* into *"3h ago · fixing the pane resume bug"*. Copy it if you want that.

**Two portability traps it exists to document:** `stat`'s flag differs between
BSD (macOS) and GNU, and `find -printf` is GNU-only. Handle both or your picker
works on one developer's machine.

### Worked adapter: Codex

`codex` mints its own ids, and its default pane shape (`codex resume --last`)
cannot take a pick. To give Codex a picker, change `resume` to accept an id first:

```jsonc
{ "id": "codex", "type": "terminal", "label": "Codex",
  "requires_bin": ["codex"],
  "argv": ["codex"],
  "resume": { "argv": ["codex", "resume", "${veld.pane.token}"] },
  "sessions": { "argv": ["scripts/veld/codex-sessions.sh"] } }
```

Then list `~/.codex/sessions` the same way as above, filtering to rows whose
recorded working directory is `$PWD`.

### Worked adapter: anything with a sessions directory

If the tool keeps one file or directory per session under a predictable path,
you already have the whole script:

```bash
#!/usr/bin/env bash
set -uo pipefail
dir="$HOME/.mytool/sessions"
[ -d "$dir" ] || exit 0
find "$dir" -maxdepth 1 -name '*.json' -print 2>/dev/null \
  | head -n 20 \
  | while read -r f; do basename "$f" .json; done
```

---

## Variables

A `sessions` command may use `${veld.root}`, `${veld.branch}`,
`${veld.branch_raw}` (`argv` only), `${veld.worktree}`, `${veld.project}`,
`${veld.username}`, `${veld.pane.id}` and `${veld.pane.label}`.

**`${veld.pane.token}` is not available here**, and the reason is worth
understanding: the lister runs to decide *which* token there will be. Referencing
it is a `veld lint` problem rather than an empty string at runtime.

`${veld.pane.id}` is the useful one — it lets one script serve several panes:

```jsonc
"sessions": { "argv": ["scripts/veld/sessions.sh", "--agent", "${veld.pane.id}"] }
```

---

## How veld runs these (and the bounds you cannot change)

**When the pane chooser is on screen**, once, for every pane declaring a lister.
Never on a timer, and never from the `+` hover menu — hovering a menu must not
start processes.

That still means veld runs a command from the repo without the user clicking the
pane it belongs to, which is what a `status` badge already does — so it is under
the same posture and the same switch:

- `stdin` closed and **no tty**, so a tool that would prompt for credentials fails
  fast instead of hanging.
- A **10-second deadline**, enforced by killing the process group.
- Output **capped**; a cut payload keeps its whole lines and drops the partial one.
- `NO_COLOR=1` and `TERM=dumb`, so a tool cannot put escape sequences in a label.
- The full argv is written to the daemon log.
- The user's machine-wide switch, **Settings → General → Let projects run their
  own status commands**, turns it off. The pane still works; the picker is not
  offered.

**Declarations come from whichever checkout `extensions.source` names — `main` by
default**, exactly as a badge's do. Same reason: this is a repo-declared command
veld runs *without you clicking the pane*, so checking out somebody's
pull-request branch must not run that branch's lister. The commands still execute
in the worktree you are looking at, with its own branch, and a relative
`argv[0]` resolves against the declaring checkout — so a `scripts/veld/…` lister
runs main's copy of the script.

The cost is the one that setting already documents: **a picker added on a branch
does not appear until it merges.** `extensions.source = worktree` is the escape
hatch for testing one before it does — and it is also the setting that hands a
hostile branch the same capability, so flip it back when you are done.

At most **8 panes per project** may declare a lister; they all run at once when
the chooser opens. Past that, `veld lint` drops the extra pickers (never the
panes).

---

## Checking your work

`veld lint` is the **only** check that a declaration took: everything under `ide`
is lenient by design, so a bad `sessions` block is a warning and a dropped picker,
never a load error.

```sh
veld lint                             # missing resume, a resume that ignores the token,
                                      # unknown keys, a forbidden variable
./scripts/veld/claude-sessions.sh     # run it yourself, from a worktree root
./scripts/veld/claude-sessions.sh | cat -A | head    # are those real tabs?
```

Three things to try before you call it done, because each one is a different
code path:

1. **A worktree with history** — the dialog lists it, and picking a row opens the
   pane on that session.
2. **A worktree with none** — the pane opens with no dialog at all.
3. **Break the script on purpose** (`exit 1` with a message on stderr) — the
   dialog still opens, still offers *Start fresh*, and shows your message.

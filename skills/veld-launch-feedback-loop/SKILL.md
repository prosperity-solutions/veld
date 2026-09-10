---
name: veld-launch-feedback-loop
description: >
  Launch the Veld feedback loop — act as the coding agent that pulls human
  feedback off Veld's in-browser overlay and works it one item at a time. Use
  when the user says "run the feedback loop", "watch for my feedback", "I'll
  review in the browser", "start collaborating on the UI", or when they want an
  agent parked on `veld feedback next`. Assumes a Veld run is already serving
  the app.
triggers:
  - run the feedback loop
  - launch feedback loop
  - start the feedback loop
  - watch for feedback
  - feedback loop
  - I'll review in the browser
  - collaborate on the UI
allowed-tools: Read, Edit, Write, Grep, Glob, Bash(veld *)
metadata:
  author: prosperity-solutions
  version: "1.0.0"
---

# Launch the Veld Feedback Loop

Veld shows the human an in-browser overlay to comment on elements, pages and
screenshots. Those comments land on a **linear queue**. Your job is to drain it —
pull the next item, fix it, reply, repeat — until the human clicks **Done**.

## Get the instructions from the CLI, then start looping

```sh
veld skills feedback
```

That document is carried by the installed veld binary, so it describes the
`veld feedback` this machine actually has: the `next` output schema, every
thread field, the reply/resolve policy, and how to attach a screenshot. Read it
before your first `veld feedback next` — the loop has one rule that is easy to
get wrong (a `timeout` result means *call again*, not *stop*) and the document
says which.

The shape, so you know what you are agreeing to:

```
loop:
  out = veld feedback next --wait --name <run> --json
  → result "item"    : work it, then `veld feedback reply <thread-id> "…"`
  → result "timeout" : call next again — nothing was waiting yet
  → result "ended"   : the reviewer clicked Done → stop
```

`next` is a pure read with no cursor, so re-invoking is free and the loop
resumes cleanly after a restart. **Do not stop after one item.** Keep pulling
until you get `"ended"`.

If there is no run yet, ask the human to start one (`veld presets` shows what
this project offers) rather than picking for them.

## If the CLI cannot answer

- **`veld skills` says "unrecognized subcommand"** — this veld predates its own
  documentation. `veld update`, then try again. Do not improvise the loop from
  the sketch above; the reply/resolve policy is the part you would get wrong.
- **`veld -V` says "command not found"** — veld is not installed and there is no
  run to pull feedback from. Say so rather than installing it for them.

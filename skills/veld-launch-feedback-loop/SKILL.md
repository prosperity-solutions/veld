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
compatibility: Requires a veld build with `veld feedback next` (PR #127+).
allowed-tools: Read, Edit, Write, Grep, Glob, Bash(veld *)
metadata:
  author: prosperity-solutions
  version: "1.0.0"
---

# Launch the Veld Feedback Loop

Veld shows the human an in-browser overlay to comment on elements, pages, and
screenshots. Those comments land on a **linear queue**. Your job: drain it —
pull the next item, fix it, reply, repeat — until the human clicks **Done**.

The queue is stateless from your side: there is **no cursor to track**. You call
`veld feedback next`, work the item it returns, then `reply`/`resolve`. That item
drops off, and the next call returns the following one.

## On invocation — start looping now

1. **Find the run.** `veld feedback next` auto-selects the only active run; if
   several are running, pass `--name <run>`. If none is active, tell the human to
   `veld start <node> --name <run>` first, then come back.
2. **Enter the loop below and keep running it until you get `"ended"`.** Don't
   stop after one item — this is a Ralph loop; you keep pulling.

```
loop:
  out = veld feedback next --wait --name <run> --json
  → result "item"    : work it (below), then reply (or resolve)
  → result "timeout" : call next again (nothing waiting yet)
  → result "ended"   : the reviewer clicked "Done" → stop the loop
```

`next --wait` blocks in the CLI (polling ~1×/sec, up to ~4 min) then returns
`timeout`. On `timeout`, **just call it again** — re-invoking is free (no cursor)
and resumes cleanly even if you were killed or the session restarted: a day-old
comment is still at the head next time you call.

## Working an item

The `item` payload has everything you need in one call:

| Field | Use |
|-------|-----|
| `thread.messages` (last human one) | What to do right now; earlier messages are context |
| `thread.scope.selector` | CSS selector — grep the codebase for it to find the source |
| `thread.scope.component_trace` | React/Vue hierarchy — the **deepest** component is usually the file to edit |
| `thread.scope.page_url` | Which route |
| `thread.messages[].screenshot` | Absolute path to a PNG — `Read` it directly |
| `thread.viewport_width` / `height` | Check for responsive issues |

Make the change in code, then reply on the thread:

```
veld feedback reply --name <run> <thread-id> "Done — <what you changed>"
```

`<thread-id>` accepts a short prefix. After you reply, the thread becomes
**blocked** — hidden from `next` — until the human responds again; a new human
message puts it back in the queue automatically. Keep replies short; the human is
reviewing in flow.

## Reply vs resolve

- **Reply** is the default — it parks the thread on the human.
- **Resolve** (`veld feedback resolve --name <run> <id>`) closes a thread. Use it
  **only when the human has explicitly approved** ("looks good", "done", "ship
  it"). When in doubt, reply and leave it open.
- Under-resolving is harmless; over-resolving closes work the human didn't sign
  off on. The human can always reopen from the overlay panel.

## Asking a question

If feedback is ambiguous, open a thread instead of guessing:

```
veld feedback ask --name <run> "Which blue — the brand token or the CTA hover?"
veld feedback ask --name <run> --page "/pricing" "Should this table scroll on mobile?"
```

Your question is blocked until the human answers, then it comes back to you.

## Stopping

The loop ends **only** when the human clicks **Done** in the overlay → `next`
returns `"ended"`. Done drains first: while any thread is still waiting on you,
`next` keeps returning `item`; `ended` fires once the queue is empty. So reply or
resolve every item. If the human adds feedback *after* Done, just run the loop
again — it picks up where it left off.

## Notes

- **One agent per run.** The loop is built for a single reviewer + single agent;
  don't run two `next` loops against the same run.
- This skill only drives the loop. For starting environments, config, or logs,
  use the main `veld` skill or `veld --help`.

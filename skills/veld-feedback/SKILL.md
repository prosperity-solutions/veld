---
name: veld-feedback
description: Collaborate with humans through Veld's in-browser feedback threads. Use when the user asks you to get feedback on UI changes, request a human review, show your work to someone, or when you need visual verification of frontend changes. Also use when iterating on design based on reviewer comments.
allowed-tools: Bash(veld feedback *), Bash(veld runs *), Bash(veld status *), Bash(veld urls *)
metadata:
  author: prosperity-solutions
  version: "3.0.0"
---

# Bidirectional Feedback with Veld

Veld injects a feedback overlay into every page it serves. Humans create threads on elements/pages, you reply and fix in real time.

## Current Runs

!`veld runs 2>&1`

## When to Use

- After visual/UI changes that need human eyes
- When unsure if a design looks right
- When the user says "show me" or "let me review"
- When iterating on frontend work

## The Loop

```
1. Make your changes
2. veld feedback listen --name <run> --json
   → Browser shows "Agent is listening"
3. Human creates threads, leaves comments
4. You receive events, fix issues
5. veld feedback answer --name <run> --thread <id> "Fixed — increased contrast"
6. veld feedback listen --name <run> --json --after <seq>
7. Repeat until session_ended event
```

## Commands

```bash
# Listen for next event (blocks until one arrives)
veld feedback listen --name dev --json
veld feedback listen --name dev --json --after 3  # only events after seq 3

# Reply to a thread
veld feedback answer --name dev --thread <id> "Done — bumped font to 2rem"

# Ask the human a question (creates a new thread)
veld feedback ask --name dev "Should the sidebar collapse on mobile too?"
veld feedback ask --name dev --page "/dashboard" "Does this layout feel right?"

# View threads
veld feedback threads --name dev --json
veld feedback threads --name dev --json --open
veld feedback threads --name dev --json --resolved
```

## Event Types

| Event | Meaning | Action |
|-------|---------|--------|
| `thread_created` | New thread with comment | Read, fix, reply |
| `human_message` | Follow-up on existing thread | Adjust, reply |
| `resolved` | Human is satisfied | Move on |
| `reopened` | Human reopened a thread | Re-examine |
| `session_ended` | Human clicked "All Good" | Exit loop |

## Key Fields in Events

| Field | Use |
|-------|-----|
| `messages[0].body` | What the human wants |
| `scope.selector` | CSS selector — find it in code |
| `component_trace` | React/Vue hierarchy — deepest component is usually the target |
| `scope.page_url` | Which route |
| `viewport_width/height` | Responsive issue? |
| `scope.type` | `element`, `page`, or `global` |

## Listen Loop Pattern

```
last_seq = 0
loop:
  event = veld feedback listen --name <run> --json --after last_seq
  last_seq = event.seq
  match event.event:
    thread_created → read, fix code, answer
    human_message  → read follow-up, adjust, answer
    resolved       → note, continue
    reopened       → re-examine
    session_ended  → exit
```

## Best Practices

- **Always pass `--after`** with the last seq to avoid reprocessing
- **Reply to threads** so the human knows you saw their feedback
- **Keep replies concise** — the human is reviewing in flow
- **Finish multi-step changes first**, then listen — don't listen mid-change
- **Use `component_trace`** — the deepest component is usually the file to edit

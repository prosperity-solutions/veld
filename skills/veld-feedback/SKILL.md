---
name: veld-feedback
description: Collaborate with humans through Veld's in-browser feedback threads. Use when the user asks you to get feedback on UI changes, request a human review, show your work to someone, or when you need visual verification of frontend changes. Also use when iterating on design based on reviewer comments.
metadata:
  author: prosperity-solutions
  version: "2.0.0"
---

# Bidirectional Feedback with Veld

Veld injects a feedback overlay into every page served through it. Humans can create threads on specific elements, pages, or globally, and you can reply, ask questions, and resolve threads — all in real time. No more blocking and waiting for batch submissions.

## When to Use This

- After making visual/UI changes that need human eyes
- When you're unsure if a design looks right
- When the user asks you to "show" them something
- When iterating on frontend work and want fast feedback loops
- After deploying a fix to verify it looks correct

## The Feedback Loop

```
You make changes
    |
veld feedback listen --name <run> --json
    |
Human sees "Agent is listening" in the browser overlay
    |
Human creates threads on elements, leaves comments
    |
You receive events, read code, fix issues
    |
You reply: veld feedback answer --name <run> --thread <id> "Fixed it"
    |
Optionally ask: veld feedback ask --name <run> "Which shade of blue?"
    |
Human reviews, replies, resolves threads
    |
Loop until human clicks "All Good" (session_ended event)
```

## Step-by-Step

### 1. Make sure a Veld environment is running

```bash
# Check if there's already a run
veld runs

# If not, start one
veld start frontend:local --name dev
```

### 2. Start listening for feedback

```bash
veld feedback listen --name dev --json
```

This blocks until a feedback event arrives. The browser overlay shows "Agent is listening" to the human. When an event arrives (thread created, message, resolve, etc.), it prints JSON and exits.

### 3. Process events

The listen command returns a single JSON event:

```json
{
  "seq": 1,
  "event": "thread_created",
  "thread": {
    "id": "abc12345-...",
    "scope": { "type": "element", "page_url": "/dashboard", "selector": "header > h1.title" },
    "origin": "human",
    "component_trace": ["App", "Layout", "Header"],
    "status": "open",
    "messages": [{ "id": "...", "author": "human", "body": "The header text is too small on mobile", "created_at": "..." }],
    "viewport_width": 1440,
    "viewport_height": 900,
    "created_at": "...",
    "updated_at": "..."
  },
  "timestamp": "..."
}
```

Event types you'll receive:

| Event | Meaning | Action |
|-------|---------|--------|
| `thread_created` | Human created a new feedback thread | Read the comment, fix the issue, reply |
| `human_message` | Human added a follow-up to an existing thread | Read it, adjust your fix, reply |
| `resolved` | Human resolved a thread (they're satisfied) | Note it, move on |
| `reopened` | Human reopened a previously resolved thread | Re-examine the issue |
| `session_ended` | Human clicked "All Good" — session is over | Exit the listen loop |

### 4. Act on feedback

For each `thread_created` or `human_message`:

1. Read the message and understand what the human wants
2. Use `scope.selector` and `component_trace` to find the right code
3. If a screenshot is present, read it to see what the human sees
4. Make the code changes
5. Reply to confirm:

```bash
veld feedback answer --name dev --thread abc12345 "Increased the font size to 2rem and added a media query for mobile"
```

### 5. Ask questions when needed

```bash
veld feedback ask --name dev "Should the sidebar also collapse on mobile, or just the header?"
```

This creates a new global thread visible to the human. They can reply in the browser overlay.

For page-specific questions:

```bash
veld feedback ask --name dev --page "/dashboard" "Does the new sidebar layout feel right?"
```

### 6. Continue listening

Pass `--after` with the seq from the last event to get only new events:

```bash
veld feedback listen --name dev --json --after 1
```

This blocks until the next event. If multiple events queued up while you were fixing things, it returns the next one immediately (in order).

### 7. The full listen loop

```
last_seq = 0
loop:
  event = veld feedback listen --name dev --json --after last_seq
  if event is null: timeout, exit
  last_seq = event.seq
  switch event.event:
    thread_created: read comment, fix code, reply with answer
    human_message: read follow-up, adjust fix, reply
    resolved: note it, continue
    reopened: re-examine
    session_ended: exit loop
```

### 8. View current threads

```bash
# All threads
veld feedback threads --name dev --json

# Only open threads
veld feedback threads --name dev --json --open

# Only resolved
veld feedback threads --name dev --json --resolved
```

## Key Fields in Thread Events

| Field | What it tells you |
|-------|-------------------|
| `messages[0].body` | What the human wants changed |
| `scope.selector` | CSS selector of the element — find it in your code |
| `component_trace` | React/Vue component hierarchy — find the right component file |
| `scope.page_url` | Which page/route the thread is about |
| `viewport_width/height` | Screen dimensions — check if it's a responsive issue |
| `scope.type` | `element`, `page`, or `global` — where the thread is anchored |

## Best Practices

### Do listen for feedback when:
- You've made visual changes and want confirmation
- You're unsure about a design decision
- The user explicitly asked you to show them the result
- You've fixed a reported visual bug and want verification

### Don't listen when:
- You're making backend-only changes with no visual impact
- The user hasn't asked for a review
- You're in the middle of a multi-step change — finish first, then listen

### Make it efficient:
- Always pass `--after` with the last seq to avoid reprocessing old events
- Reply to threads so the human knows you saw their feedback
- Keep replies concise — the human is reviewing in flow
- If a comment is unclear, ask a clarifying question via `answer`
- Keep change sets small and focused

### When reading component traces:
- The trace shows the React/Vue component hierarchy: `App > Layout > Sidebar > NavItem`
- The deepest component is usually the one to edit
- Search for the component name in the codebase to find the source file

## Example Workflow

```
User: "Make the dashboard header match the new brand colors"

You:
1. Find the header component
2. Update the colors
3. Tell the user what you changed
4. Run: veld feedback listen --name dev --json
5. Event: thread_created on header > h1.page-title
   "The gradient looks good but the text is hard to read against it"
   Components: App > DashboardLayout > Header
6. You adjust the text color/contrast
7. Run: veld feedback answer --name dev --thread t_1 "Increased contrast — white text with subtle shadow"
8. Run: veld feedback listen --name dev --json --after 1
9. Event: resolved (thread t_1)
10. Run: veld feedback listen --name dev --json --after 2
11. Event: session_ended
12. Done.
```

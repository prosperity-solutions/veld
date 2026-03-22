# Veld Feedback Reference

Veld injects a feedback overlay into every page it serves. Humans create threads on elements/pages, you reply and fix in real time.

## When to Listen

- After visual/UI changes that need human eyes
- When unsure if a design looks right
- When the user says "show me" or "let me review"
- **Don't** listen mid-change — finish multi-step work first

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
# Listen (blocks until event arrives)
veld feedback listen --name dev --json
veld feedback listen --name dev --json --after 3

# Reply
veld feedback answer --name dev --thread <id> "Done — bumped font to 2rem"

# Ask (creates a new thread visible to the human)
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
| `component_trace` | React/Vue hierarchy — deepest component is usually the file to edit |
| `scope.page_url` | Which route |
| `viewport_width/height` | Check for responsive issues |
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

## Interactive Controls

Instead of asking "what value?" — send a control. The human scrubs a slider, picks a color, toggles an option, and you get the exact value back.

### Sending Controls

```bash
# Attach controls to a question
veld feedback ask --name dev \
  --controls '[
    {"type":"slider","name":"duration","value":200,"min":50,"max":2000,"step":10,"unit":"ms"},
    {"type":"select","name":"easing","value":"ease-out","options":["linear","ease-in","ease-out","ease-in-out"]},
    {"type":"color","name":"accent","value":"#3b82f6"},
    {"type":"button","name":"replay","label":"Replay animation"}
  ]' \
  "Try adjusting the animation:"

# Or attach controls to a reply
veld feedback answer --name dev --thread <id> \
  --controls '[...]' \
  "Here are some options to tune:"
```

### Control Types

| Type | Fields | Renders as |
|------|--------|------------|
| `slider` | `name, value, min, max, step?, unit?, label?` | Range slider with value readout |
| `number` | `name, value, min?, max?, step?, unit?, label?` | Number input with Alt+drag scrubbing |
| `select` | `name, value, options, label?` | Dropdown |
| `color` | `name, value, label?` | Color picker |
| `text` | `name, value, placeholder?, label?` | Text input |
| `toggle` | `name, value, label?` | Checkbox |
| `button` | `name, label` | Action button |

### XY Pad Fusion

The human can drag two numeric controls together to create a 2D exploration surface (XY pad). This is **not** a control type you send — it's a UI affordance the human triggers.

**To enable fusion**, a numeric control must have both `min` and `max` defined. Controls without bounds won't get a fuse grip.

Good — fusable:
```json
{"type":"slider","name":"duration","value":200,"min":50,"max":2000,"step":10,"unit":"ms"}
{"type":"number","name":"overshoot","value":0,"min":-1,"max":1,"step":0.01}
```

Not fusable (no `min`/`max`):
```json
{"type":"number","name":"count","value":5}
```

**Design for fusion**: when two parameters are related (duration + easing strength, width + height, hue + saturation), send them as adjacent bounded numeric controls. The human decides whether to fuse them — you don't need to know or ask.

### Receiving Applied Values

When the human clicks "Apply values", you receive a message on the thread:

```
Applied values: {"duration":340,"easing":"ease-out","accent":"#2563eb"}
```

Parse the JSON after "Applied values: " to get all final values. Then replace the hook/binding in code with the chosen constants and commit.

### Binding Controls to Application Code

Drop the appropriate template into the project so controls take effect in real time:

| Stack | Template | Usage |
|-------|----------|-------|
| React | `use-veld-control.react.tsx` | `const val = useVeldControl("name", default)` |
| Vue 3 | `use-veld-control.vue.ts` | `const val = useVeldControl("name", default)` |
| jQuery | `veld-control.jquery.js` | `$.veldControl("name", default, cb)` |
| Vanilla JS | `veld-control.vanilla.js` | `veldControl("name", default, cb)` |

Templates are in `skills/veld/templates/`. Copy the one matching the project's stack. When the human clicks Apply, replace the hook call with the chosen constant value.

## Best Practices

- **Always pass `--after`** with the last seq to avoid reprocessing
- **Reply to threads** so the human knows you saw their feedback
- **Keep replies concise** — the human is reviewing in flow
- **Use `component_trace`** — the deepest component is usually the file to edit
- **Always set `min` and `max`** on numeric controls — without bounds, the control cannot be fused into an XY pad and Alt+drag scrubbing has no limits, making it harder to use
- **Group related controls** — put parameters that affect the same visual outcome adjacent to each other (e.g. duration next to easing strength) so the human can fuse them into a 2D exploration surface
- **Prefer `slider` for bounded ranges** and `number` for open-ended values with optional bounds

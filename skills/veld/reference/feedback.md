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
   → Returns ALL pending events at once (batch mode)
   → Threads are auto-claimed for this agent
3. Human creates threads, leaves comments
4. You receive events, fix issues (in parallel if needed)
5. veld feedback release --name <run> --thread <id> "Fixed — increased contrast"
   → Posts comment + releases claim atomically
6. veld feedback listen --name <run> --json --after <last_seq>
7. Repeat until session_ended event
```

## Commands

```bash
# Listen (blocks until events arrive — returns ALL pending events as a batch)
veld feedback listen --name dev --json
veld feedback listen --name dev --json --after 3
veld feedback listen --name dev --json --agent my-agent   # explicit agent name
veld feedback listen --name dev --json --no-batch          # legacy: single event only

# Reply
veld feedback answer --name dev --thread <id> "Done — bumped font to 2rem"

# Release a claimed thread with a status comment (atomic: comment + release)
veld feedback release --name dev --thread <id> "Fixed — bumped font to 2rem"
veld feedback release --name dev --thread <id> --agent my-agent "Increased contrast ratio"
veld feedback release --name dev --thread <id>   # release without comment

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
| `thread_claimed` | Thread claimed by an agent | (browser-only, not returned to agents) |
| `thread_released` | Thread released by agent/UI | (browser-only, not returned to agents) |

## Key Fields in Events

| Field | Use |
|-------|-----|
| `messages[0].body` | What the human wants |
| `scope.selector` | CSS selector — find it in code |
| `component_trace` | React/Vue hierarchy — deepest component is usually the file to edit |
| `scope.page_url` | Which route |
| `viewport_width/height` | Check for responsive issues |
| `scope.type` | `element`, `page`, or `global` |
| `claimed_by` | Agent ID that claimed this thread |
| `claimed_at` | When the claim was created |

## Listen Loop Pattern

```
last_seq = 0
loop:
  batch = veld feedback listen --name <run> --json --after last_seq
  last_seq = batch.last_seq
  for each event in batch.events:
    match event.event:
      thread_created → read, fix code, release "Fixed X"
      human_message  → read follow-up, adjust, release "Adjusted Y"
      resolved       → note, continue
      reopened       → re-examine, release "Re-examined Z"
      session_ended  → exit
```

Threads are auto-claimed when returned by `listen`. Release them with `veld feedback release` after you're done.

## Multi-Agent Workflow

Multiple agents can listen on the same run. Each agent gets a unique ID (default: `agent-<pid>`, or set with `--agent`).

```
Agent-1: veld feedback listen --json --agent css-fixer
Agent-2: veld feedback listen --json --agent layout-agent
```

When `listen` returns events, it auto-claims the referenced threads for that agent. Other agents calling `listen` will skip already-claimed threads. This prevents duplicate work.

After finishing work on a thread, release it with a status comment:
```
veld feedback release --thread <id> --agent css-fixer "Fixed contrast ratio to 4.5:1"
```

Humans can also release stale claims via the "Release" button in the browser overlay.

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

- **Always pass `--after`** with `batch.last_seq` to avoid reprocessing
- **Release threads** after finishing work — don't hold claims indefinitely
- **Reply to threads** so the human knows you saw their feedback
- **Keep replies concise** — the human is reviewing in flow
- **Use `component_trace`** — the deepest component is usually the file to edit
- **Always set `min` and `max`** on numeric controls — without bounds, the control cannot be fused into an XY pad and Alt+drag scrubbing has no limits, making it harder to use
- **Group related controls** — put parameters that affect the same visual outcome adjacent to each other (e.g. duration next to easing strength) so the human can fuse them into a 2D exploration surface
- **Prefer `slider` for bounded ranges** and `number` for open-ended values with optional bounds

## Troubleshooting

Common issues when integrating veld with web applications:

### WebSocket / HMR not working (page not interactive)

**Symptom**: Page loads but buttons don't work, console shows "WebSocket connection failed" for `/_next/webpack-hmr` or similar HMR endpoints.

**Cause**: Dev servers (Next.js, Vite) reject WebSocket upgrades when the `Origin` header doesn't match `localhost`. The browser sends `Origin: https://your-app.run.project.localhost` but the dev server only accepts `Origin: http://localhost:<port>`.

**Fix (veld side)**: veld 6.2+ strips the `Origin` header from upstream requests automatically. If you're on an older version, run `veld update`.

**Fix (app side)**: If using Next.js, add your veld domain pattern to `allowedDevOrigins` in `next.config.js`:
```js
allowedDevOrigins: ["*.localhost", "*.preview.life.li"]
```
Note: this only fixes the dev overlay CORS check, not the WebSocket origin check (which is a separate Next.js issue). The veld-side fix is more reliable.

### Content Security Policy (CSP) blocking connections

**Symptom**: Page loads but fetch/XHR/WebSocket calls fail. Console shows "Refused to connect" CSP violations.

**Cause**: The app's CSP `connect-src` directive only allows `'self'` and `localhost:<port>`. When served through veld's proxy on a different hostname, connections to `'self'` work but any hardcoded `localhost` references in the CSP won't match the proxy origin.

**Fix**: Update the app's CSP to include the veld proxy domain:
```
connect-src 'self' wss://*.localhost ws://*.localhost https://*.localhost
```
Or for non-localhost domains:
```
connect-src 'self' wss://*.dev.preview.life.li https://*.dev.preview.life.li
```

### Veld overlay disappears after page load (React hydration)

**Symptom**: Veld toolbar flashes briefly then disappears. React hydration warning in console about mismatched content.

**Cause**: On veld versions before 6.2, the overlay was mounted on `<body>`. React hydrates `<body>` and removes elements it didn't render on the server.

**Fix**: Update veld — version 6.2+ mounts the overlay on `<html>` (outside React's hydration scope).

### Port conflicts

**Symptom**: `veld start` fails with "port already in use" or the health check times out.

**Cause**: Veld allocates ports in the 19000–29999 range. Another process may be using the allocated port.

**Fix**: Check what's using the port: `lsof -i :<port>`. Kill the process or let veld pick a different port (restart the run).

### HTTPS certificate warnings

**Symptom**: Browser shows "Your connection is not private" or certificate errors.

**Fix**: Run `veld setup privileged` (or `veld setup unprivileged`) to generate and trust the local CA certificate. If already set up, run `veld doctor` to check CA trust status.

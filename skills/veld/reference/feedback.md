# Veld Feedback Reference

Veld injects a feedback overlay into every page it serves. A human leaves comments on elements, pages, or screenshots; you pull them off a linear queue, fix them, and reply — one item at a time.

The queue is **stateless from your side**: there is no cursor to track. You call `veld feedback next`, work the item it returns, then `reply` or `resolve` it. That item drops off the queue and the next call returns the following one.

## When to Run the Loop

- After visual/UI changes that need human eyes
- When unsure whether a design looks right
- When the user says "show me" or "let me review"
- **Don't** start mid-change — finish multi-step work first, then enter the loop

## The Loop

```
loop:
  out = veld feedback next --wait --name <run> --json
  → result "item"    : work it, then `veld feedback reply <id> "..."` (or resolve)
  → result "timeout" : call next again
  → result "ended"   : the reviewer clicked "Done" → stop
```

`next --wait` blocks in the CLI — polling the local queue about once a second,
up to ~4 min — until an item is waiting, the reviewer ends the session, or it
times out. On `timeout`, just call it again —
because there is no cursor, re-invoking is free and always returns the current
head of the queue. This survives the command being killed or the session
resuming later: a day-old comment is still at the head next time you call.

There is exactly one way the loop ends: the reviewer clicks **Done** in the
overlay, which surfaces as `result: "ended"`. Done drains first — while any
thread is still waiting on you, `next` keeps returning `item`; `ended` only
fires once the queue is empty. So reply or resolve every item; if one genuinely
can't be actioned, the reviewer resolves it from the threads panel.

## Commands

```bash
# Get the next waiting item (blocks with --wait). Pure read — safe to re-run.
veld feedback next --wait --name dev --json

# Reply → the thread becomes "blocked" and drops off the queue until the
# human responds again (which puts it back automatically).
veld feedback reply --name dev <thread-id> "Done — bumped the font to 2rem"

# Resolve → close the thread. Only on explicit human approval (see below).
veld feedback resolve --name dev <thread-id>

# Ask → open a new thread with a question for the reviewer.
veld feedback ask --name dev "Should the sidebar collapse on mobile too?"
veld feedback ask --name dev --page "/dashboard" "Does this layout feel right?"

# Inspect threads (debugging / overview).
veld feedback threads --name dev --json
veld feedback threads --name dev --json --open
veld feedback threads --name dev --json --resolved
```

`<thread-id>` accepts a short prefix (git-style) — the first few characters are
usually enough.

## The `next` Output

```json
{
  "result": "item",
  "thread": {
    "id": "a1b2c3d4",
    "scope": {
      "type": "element",
      "page_url": "https://app.dev.veld.localhost/",
      "selector": "header > nav > button.cta",
      "position": { "x": 120, "y": 40, "width": 96, "height": 32 },
      "element_text": "Get started",
      "source_file": "src/components/CtaButton.tsx",
      "source_line": 42
    },
    "component_trace": ["App", "Header", "CtaButton"],
    "viewport_width": 1440,
    "viewport_height": 900,
    "messages": [
      { "author": "human", "body": "This button is too small", "screenshot": "/abs/path/.veld/tmp/screenshots/dev/ss_….png", "created_at": "…" },
      { "author": "agent", "body": "Bumped it to 40px", "created_at": "…" },
      { "author": "human", "body": "still too small", "created_at": "…" }
    ]
  }
}
```

- `result` is one of `"item"`, `"timeout"`, `"ended"`. On `timeout`/`ended`
  there is no `thread`.
- Everything you need to act is in one call — full message history, scope, and
  screenshots.
- **`screenshot` is an absolute file path** — read the PNG directly.

| Field | Use |
|-------|-----|
| `messages` (last human one) | What the human wants right now |
| `scope.selector` | CSS selector — find it in code |
| `scope.element_text` | Visible text of the element (middle-truncated) — use with the selector when it's ambiguous (e.g. matches several similar elements) |
| `scope.source_file` / `scope.source_line` | Best-effort file:line of the element's JSX/template tag (React ≤18 dev builds; Vue gives file only). Absent in production builds, and on React 19+, which dropped the dev-source metadata this reads — fall back to the selector/component_trace |
| `component_trace` | React/Vue hierarchy, nearest-ancestor-last and capped to ~12 entries — the deepest component is usually the file to edit |
| `scope.page_url` | Which route |
| `scope.type` | `element`, `page`, or `global` |
| `viewport_width/height` | Check for responsive issues |
| `messages[].screenshot` | Absolute path to a PNG — read it |

## Queue Semantics

- `next` returns the **oldest waiting thread**: open, with the latest message
  from the human. Reply and it becomes *blocked* (hidden) until the human
  writes again; then it re-enters the queue automatically. You never manage
  this state — it's derived from who spoke last.
- Order is FIFO — oldest waiting item first. A fresh human comment on an old
  thread moves it to the back, so you always drain the oldest work first.
- `next` is a pure read: calling it repeatedly returns the same item until you
  `reply` or `resolve`.

## Resolve Policy

- **Reply** (`veld feedback reply`) is the default. It parks the thread on the
  human and drops it off your queue.
- **Resolve** (`veld feedback resolve`) closes a thread. Use it **only when the
  human has explicitly approved** ("looks good", "done", "ship it"). When in
  doubt, reply and leave it open.
- Over-resolving is recoverable: the reviewer can reopen a resolved thread from
  the panel. (If your reply was its last message, they'll also add a comment —
  that new human message is what re-enters it into the queue.) Under-resolving
  (leaving approved threads open) is worse: they linger as blocked zombies.

## Single Agent

Run one agent per run. The loop is built for a single reviewer iterating with a
single agent — the 80% case, and the one that stays reliable. There is no thread
claiming, locking, or multi-agent coordination.

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

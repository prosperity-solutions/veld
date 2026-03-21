# Client-Side DevTools — Design Manifest

> **Status:** Draft / RFC — captures the vision and design decisions so far.
> No code has been written yet.

Veld already injects a feedback overlay via Caddy's `replace_response` plugin. The same
injection pipe can carry a full browser-to-agent telemetry system — giving agents
eyes, ears, and a nervous system for the running application.

---

## Core Insight

Veld has accidentally built a **universal browser-to-agent telemetry bridge**:

```
Caddy script injection  →  Daemon HTTP API  →  CLI long-poll  →  Agent
```

Any JavaScript that runs in the page can stream data back to the agent through this
pipe. The question is: what data, and how should agents consume it?

---

## Three Primitives

After exploring many options, the design converges on three interaction primitives
with distinct data shapes, cost profiles, and UX models.

### 1. Streams — `veld logs --client`

**What:** Console logs, errors, uncaught exceptions, network requests/responses.

**When active:** Always-on from page load. Cheap — just monkey-patches on `console.*`,
`fetch`, `XMLHttpRequest`, and `window.onerror` / `onunhandledrejection`.

**Storage:** Ring buffer in daemon (last N entries, bounded memory). No disk persistence
by default.

**CLI interface:**
```bash
veld logs --name dev --client                # all client logs, streaming
veld logs --name dev --client --errors       # errors only
veld logs --name dev --client --network      # network traffic only
veld logs --name dev --client --since 2m     # query: last 2 minutes
veld logs --name dev --client --grep "404"   # filter
veld logs --name dev --client --json         # structured output for agents
```

**Why not snapshots?** Logs and network traffic are continuous — they happen whether
you're watching or not. Streaming is the natural model. The ring buffer makes
historical queries cheap.

**Rich context on errors:** A console error entry isn't just a message. It carries:
- Stack trace with source locations
- Component trace (React fiber / Vue instance walk)
- DOM excerpt around the erroring element
- Recent network calls (last N before the error)
- Current page URL and viewport

This means agents often don't need a separate "inspect" step — the error log is
already rich enough to act on.

### 2. Feedback — `veld feedback --wait` (already built)

**What:** Human-initiated comments, element selection, screenshots.

**Direction:** Human → Agent.

**Enhancement opportunity:** When a human selects an element and comments, auto-attach
richer context: DOM neighborhood, computed styles on the selected element, component
state, related recent network calls. The feedback payload becomes an inspection
for free.

### 3. Cowork — `veld cowork`

**What:** Agent-initiated interactive session. The agent can:
- Execute arbitrary JavaScript in the browser page context
- Send messages/instructions to the human via the overlay UI
- Receive responses from the human

**This is the big idea.** Instead of building N specialized inspectors (localStorage
inspector, component tree walker, heap profiler wrapper...), build **one general-purpose
remote execution bridge** and let the model craft the exact query it needs.

The model already knows every browser API. It knows `PerformanceObserver`,
`MutationObserver`, `getComputedStyle`, `indexedDB.databases()`, all of it. We just
need to give it a way to execute JS in the page context.

**CLI interface:**
```bash
# Start a cowork session (human must accept in overlay)
veld cowork --name dev

# Execute JS in browser context
veld cowork --name dev --exec 'document.querySelectorAll("form input").length'
# → 7

veld cowork --name dev --exec 'window.__NEXT_DATA__?.props?.pageProps'
# → {user: {id: 123, plan: "free"}, cart: {items: [...]}}

veld cowork --name dev --exec '
  const violations = [];
  document.querySelectorAll("img:not([alt])").forEach(img => {
    violations.push({src: img.src, selector: img.className});
  });
  return violations;
'

# Send a message to the human in the overlay
veld cowork --name dev --say "Navigate to the checkout page"

# End the session
veld cowork --name dev stop
```

**Why this subsumes most "inspect" features:**
- localStorage dump → `--exec 'JSON.stringify(localStorage)'`
- Component tree → `--exec` with React fiber traversal
- Computed styles → `--exec 'getComputedStyle(...)'`
- a11y check → `--exec` with axe-core-style DOM scan
- Any future browser API → already supported, no Veld changes needed

**Overlay UX in cowork mode:**
- Visual indicator that a cowork session is active (colored border / persistent banner)
- Chat-like panel showing agent messages
- Optionally: live feed of executed code (expandable log for transparency)
- Kill switch — human clicks "End Session" and it's over
- Acceptance prompt when session starts (like a screen-share request)

**Security:**
- Only works on localhost / .localhost domains (already enforced by Veld)
- Human must accept the session before any code executes
- Human can end the session at any time
- Visual indicator always visible — no silent background execution

### Performance Recording — A Cowork Recipe

Performance profiling isn't a fourth primitive — it's a **recording session** that can
be built on top of cowork or as a lightweight standalone mode:

```bash
veld perf --name dev start                    # start recording
veld perf --name dev start --duration 30s     # auto-stop after 30s
veld perf --name dev stop                     # stop and print report
veld perf --name dev report --json            # get results
```

Under the hood, starting a perf recording injects `PerformanceObserver` for LCP, CLS,
INP, TTFB, long tasks, and memory pressure. Collects entries in the page, pushes
summary to daemon on stop. No overhead when not recording.

This keeps the MacBook cool — no ambient performance collection.

---

## Architecture

### Data flow

```
Browser (injected JS)
  │
  ├── Streams: POST /__veld__/devtools/stream   (continuous, fire-and-forget)
  ├── Cowork:  GET  /__veld__/devtools/poll      (JS polls for exec requests)
  │            POST /__veld__/devtools/result     (JS returns exec results)
  └── Perf:    POST /__veld__/devtools/perf      (push recording data on stop)
  │
Daemon (port 19899)
  │
  ├── Ring buffer for streams (bounded memory)
  ├── Request/response queue for cowork exec
  └── Perf recording storage
  │
CLI
  │
  ├── veld logs --client        → GET  /__veld__/devtools/stream (SSE or long-poll)
  ├── veld cowork --exec '...'  → POST /__veld__/devtools/exec
  │                               GET  /__veld__/devtools/exec/result (long-poll)
  ├── veld cowork --say '...'   → POST /__veld__/devtools/say
  └── veld perf report          → GET  /__veld__/devtools/perf
```

### Injection strategy

Same as feedback: Caddy's `replace_response` injects a `<script>` tag. The devtools
JS is a separate file (`/__veld__/devtools/devtools.js`) loaded alongside the existing
feedback overlay. Lightweight boot — registers stream hooks immediately, but cowork
and perf capabilities are dormant until activated.

### What about Chrome DevTools Protocol (CDP)?

CDP is the wire protocol Chrome uses for its own DevTools. It exposes everything:
console, network (with full response bodies), DOM, JS execution, heap snapshots,
CPU profiles, accessibility tree.

**However:** CDP only works if Chrome is launched with `--remote-debugging-port`. A
normal Chrome instance that someone clicked open from their dock has no CDP access.
You cannot attach after the fact.

**Decision: Injection is the primary path.** It works in every browser, every tab,
no special launch flags. The human just opens the URL and it works.

**CDP is a future optional turbo mode** for when agents drive the browser via Playwright
(which launches its own Chromium with CDP already enabled). In that case, the agent
gets richer data — full network response bodies, heap snapshots, CPU profiles — for
free. But it's additive, not required.

The daemon API is designed so CDP can slot in later as an alternative data source
behind the same endpoints. The CLI and agent interface stay identical.

---

## What We're NOT Building

- **localStorage/cookie dumps** — sensitive data risk (auth tokens, API keys). If an
  agent needs a specific value, it can use cowork `--exec` for a targeted query.
  Network inspector already shows auth headers per-request.
- **Always-on performance collection** — too expensive. Perf is explicit start/stop.
- **Browser extension** — injection via Caddy is simpler and browser-agnostic.
- **Service Worker inspection** — niche, can be done via cowork `--exec` if needed.

---

## Impact-Ordered Feature List

### Tier 1 — Transforms what agents can do autonomously
1. **Console & Error Capture** (stream) — agents see runtime errors as they happen
2. **Network Traffic Inspector** (stream) — agents see API calls, status codes, timing
3. **Cowork / Remote Execution** — agents can inspect anything via JS execution

### Tier 2 — Multiplies agent effectiveness on specific tasks
4. **Performance Recording** — explicit start/stop profiling sessions
5. **Rich Feedback Payloads** — auto-attach inspection context to human feedback
6. **Component & State Bridge** — cowork recipe for React/Vue/Svelte tree walking

### Tier 3 — High value for specific debugging scenarios
7. **Accessibility Auditing** — cowork recipe running axe-core patterns
8. **Visual Regression / Screenshot Diffing** — before/after with changed regions
9. **User Interaction Replay** — event sequence recording

### Tier 4 — Niche
10. WebSocket/SSE monitoring
11. Form state inspection
12. Router/navigation state
13. Memory/heap profiling (via cowork or CDP future)
14. i18n/translation coverage

---

## Build Order

**Phase 1:** Streams infrastructure + console/error capture + network capture
**Phase 2:** Cowork mode (remote execution + human messaging overlay)
**Phase 3:** Performance recording
**Phase 4:** Rich feedback payloads (enhance existing feedback with auto-context)
**Phase 5:** CDP integration (optional turbo mode for Playwright scenarios)

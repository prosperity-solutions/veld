# RFC: Feedback System Redesign — Linear Machine View

**Status:** Proposed · **Supersedes:** `INTERACTIVE_TOOLS_MANIFEST.md` · **Branch:** `rethink-feedback-system-interaction`

One-line: keep the human-facing thread model, add a **linear, stateless, single-agent queue** for the machine, and delete everything else.

---

## 1. Philosophy

Three principles, in priority order:

1. **The consumer is an LLM.** The overlay and CLI exist to feed a coding agent crisp signal: *which element, which page, a screenshot, and words.* Any feature that produces human-to-human signal an agent reads poorly (doodles, spotlights, tuned sliders sent back as JSON) is noise, not value.
2. **One agent, one loop, bulletproof.** The 80% case is a single human iterating with a single agent. Optimize for that being flawless. Parallelism is a non-goal (see §9).
3. **Strip-and-fix, not rewrite.** The durable substrate — append-only event log, per-thread file store, flock integrity, short-id resolution, screenshot storage — already works and is tested. We keep the bones, remove the coordination layer, fix the loop, cut the tools.

### Why the current system fails (diagnosis, not blame)

The linear machine view *already exists* — it's the append-only event log. It failed for three concrete reasons, none of which is the data model:

- **The agent CLI is stateful.** `listen --after N` makes the agent carry a sequence cursor across calls. LLMs drop it. This is the primary failure.
- **`listen` exits on timeout and ends the session.** New human comments then sit unwatched until the human manually re-invokes the agent ("please listen again").
- **`listen` mutates on read** (auto-claims threads). Reads must be pure.

Plus a documentation split: the installed `SKILL.md` describes a legacy flow that no longer matches the CLI.

---

## 2. The Model: Two Views Over One Store

The data is unchanged conceptually — **threads with messages**. Two projections sit on top:

- **Human view** (overlay): thread-based. Natural for people. Comment, reply, resolve, reopen.
- **Machine view** (CLI): a **linear todo queue** derived from thread state. No cursor, no threads to recall — the agent queries the head, acts, repeats.

### Thread states are *derived*, not stored

We do **not** add a `blocked` status field. State is a pure function of existing data:

| Derived state | Definition | In agent queue? |
|---|---|---|
| **Waiting** | `status=Open` AND latest message author is **Human** (or no agent reply yet) | **Yes** |
| **Blocked** | `status=Open` AND latest message author is **Agent** | No (waiting on human) |
| **Resolved** | `status=Resolved` | No |

The unblock rule is free: a human reply flips `latest author → Human`, so a Blocked thread becomes Waiting and re-enters the queue automatically. No one has to "un-block" anything.

Agent-initiated threads (`ask`) fit the same rule: created with `latest author=Agent` → Blocked → hidden from the agent's own queue → human answers → Waiting → reappears.

---

## 3. The Agent Loop

The entire agent-facing contract is one command with three outcomes and **no arguments to track**:

```
loop:
  out = veld feedback next --wait --json     # blocks (local poll ~1×/sec), no cursor
  → ITEM     : work it, then `veld feedback reply <id> "..."`   (or resolve)
  → TIMEOUT  : call it again
  → ENDED    : human clicked "Done" → stop
```

Properties that make this robust:

- **Stateless.** `next` is a pure read of the queue head. Same item on every call until `reply`/`resolve` moves the head. Idempotent by construction.
- **Crash-safe / resumable.** The agent can be killed, time out, or resume next week — it just calls `next` again and gets the current head. A day-old comment is still there. "Continue tomorrow" needs zero restore logic.
- **Blocks in the CLI (polls the local queue ~1×/sec), not a `sleep` you manage.** `--wait` parks the agent on one command instead of telling it to `sleep` (which invites it to wander off and decide it's "done").
- **Survives Bash timeouts.** `--wait` blocks up to a relaxed internal timeout (~4 min, under Claude Code's 10-min Bash ceiling), then returns `TIMEOUT`. Because there's no cursor, re-invocation is free and idempotent — the skill says one line: *"got `TIMEOUT`? call it again."*

### Terminal condition

The loop stops on exactly **one** signal: the human clicks **Done** in the overlay → `ENDED`. There is **no auto-exit** on empty queue and **no time-based auto-close** of threads. Clicking Done stops the *current loop*; it does not resolve open threads — they persist and reappear next session.

---

## 4. CLI Surface

### Kept / new

| Command | Purpose | Notes |
|---|---|---|
| `veld feedback next --wait --json` | Return the head of the queue (oldest Waiting thread) | Pure read. `--wait` blocks; without it, returns immediately (`ITEM`/`TIMEOUT`/`ENDED`). |
| `veld feedback reply <id> "..."` | Post an agent reply → thread becomes Blocked | Short-prefix `<id>` resolution kept. |
| `veld feedback resolve <id>` | Agent resolves a thread | Skill-gated: only on explicit human approval (see §7). |
| `veld feedback ask [--page <url>] "..."` | Agent opens a new thread (clarifying question) | Kept for simplicity. |
| `veld feedback threads [--open\|--resolved] [--json]` | List threads (debug / human inspection) | Unchanged. |

### Removed

- `listen` and **the `--after` / seq cursor** — replaced by `next`. This is the headline change.
- **`--controls`** on `answer`/`ask` — the interactive-controls subsystem is deleted (§8).
- **Auto-claim on read** — `next` never mutates.
- `claim` / `release` / all thread-claiming — deleted (§9).

Naming: `next` is the primary verb (self-documenting: "give me the next thing") and pairs with `reply`/`resolve`/`ask`. `--wait` aligns with the direction already shown in `AGENTS.md`.

---

## 5. `next --json` Payload

One call must be enough to act — the agent should never need a second query for context. Schema (illustrative):

```json
{
  "result": "item",
  "thread": {
    "id": "a1b2c3d4",
    "scope": {
      "type": "element",
      "page_url": "https://website.dev.veld.localhost/",
      "selector": "header > nav > button.cta",
      "position": { "x": 120, "y": 40, "width": 96, "height": 32 }
    },
    "viewport_width": 1440,
    "viewport_height": 900,
    "messages": [
      { "author": "human", "body": "This button is too small", "screenshot": "/abs/path/.veld/feedback/dev/screenshots/ss_….png", "created_at": "…" },
      { "author": "agent", "body": "Bumped to 40px height", "created_at": "…" },
      { "author": "human", "body": "still too small", "created_at": "…" }
    ]
  }
}
```

- `result` ∈ `"item" | "timeout" | "ended"`.
- **Screenshots are absolute file paths**, so the agent can `Read` the PNG directly.
- Full message history ships every time (thread is small; statelessness beats delta-optimization).
- Scope is one of `element` (selector + position), `page` (url), or `global`.

---

## 6. Queue Semantics

- **Order: FIFO, oldest-first** by thread's last-activity timestamp. The agent drains the oldest Waiting thread first.
- **New human comment on an old thread → goes to the back**, not the head. Predictable draining beats recency; no starvation. (With 1–3 open items this rarely matters; predictability matters more.)
- Resolved and Blocked threads are excluded from `next`. They remain visible in `threads` and the overlay panel.

---

## 7. Resolve Policy (skill-level, not enforced in CLI)

The CLI exposes `resolve`; the *policy* lives in the skill:

- Agent resolves **only on explicit human approval** ("looks good", "done", "ship it").
- **Any ambiguity → `reply`, leave open.**
- Over-resolving is low-risk: a human reply on a Resolved thread reopens it (new human message → Waiting → back in queue).
- Without agent-resolve, an approved thread becomes a Blocked-but-Open zombie forever — hence resolve exists.

---

## 8. Tool Cull

Final overlay toolbar: **element · page · screenshot · threads · done.**

| Feature | Verdict |
|---|---|
| Comment on element (picker) | **Keep** — core |
| Comment on page | **Keep** — core |
| Screenshot (rectangle) | **Keep** — core (rework, §10) |
| Threads panel (view/reply/resolve/reopen) | **Keep** — human's queue view |
| "Done" button | **Keep** — the `ENDED` signal, load-bearing |
| Theme / Hide / Management-UI link | **Keep** — trivial infra |
| **Draw module** (pen, eraser, spotlight, blur/redact, numbered pins, shape-snap, colors, widths, undo) | **Delete — entire `draw-overlay/` module** |
| **Controls injection** (slider, number, select, color, text, toggle, button) | **Delete — entire controls/registry subsystem** |
| **XY-pad fusion** | **Delete** |

Rationale: doodles and tuned widgets are the weakest signal you can hand an LLM. The element picker already does "point at this"; screenshot + text already does "this looks wrong here." Two whole frontend subsystems disappear.

Future re-entry (only if demanded): a **single arrow** annotation primitive — never the suite. Blur/redact is intentionally dropped (secrets should not be on screen as plaintext in the first place).

---

## 9. Concurrency: Kill Claiming, Keep Locks

Two different things — do not confuse them:

- **Delete the coordination layer:** claim/release, `claimed_by`/`claimed_at`, `ThreadClaimed`/`ThreadReleased` events, auto-claim-on-listen, force-release UI. This is the parallelism tax on the single-agent case.
- **Keep the integrity layer:** flock write-locks on thread files and the seq counter. These prevent file corruption when a human and an agent write concurrently. They stay.

**Parallelism / multi-agent is an explicit non-goal for v1.** No sub-agent orchestration in the core skill — that reintroduces the locks-and-coordination complexity this redesign removes. If real demand appears, it ships later as a separate optional skill, never baked into the core loop.

---

## 10. UI Fixes (inside the Keep set)

### Screenshot — freeze-first (fixes the offset/shift)

Root cause: current code draws the rectangle *first*, then acquires `getDisplayMedia` *after*, so the share banner shifts the layout between selection and capture — selection and capture happen in different layouts.

Fix — invert the order:

```
1. click screenshot
2. getDisplayMedia → pick tab (dialog; the only web API for tab capture)
3. grab ONE frame → freeze to bitmap, stop stream immediately (banner vanishes)
4. show the frozen bitmap fullscreen
5. draw rectangle ON the frozen image
6. crop from the bitmap
```

Selection and capture are now the same pixels → shift is impossible, not merely reduced. The banner blinks for one frame. The DPR / `innerWidth` mapping is retained but now applied to one consistent frame (selection and crop share it), so the cross-layout offset is gone. (Historical note: the original "share continuously, select, stop" flow was dropped because of the persistent banner; freeze-first gives a good experience without a lingering banner.)

### Element picker — dark backdrop with cutout

In picker mode, dim the whole page and punch out the hovered element. One node, no SVG, no four-div framing:

```css
/* a single div sized to the hovered element's bounding rect */
box-shadow: 0 0 0 9999px rgba(0, 0, 0, 0.45);
pointer-events: none;
```

The massive box-shadow *is* the backdrop; the div's own box is the transparent cutout. Update its top/left/width/height on hover. (Same pattern intro.js / Shepherd use.)

---

## 11. Migration

- **No data migration.** After update, old feedback data need not carry over.
- **One hard rule:** old/incompatible state files must never *crash* the new code. On deserialize failure, **skip-and-log**, never panic.
- Runs are per-`run_name` directories, so old runs simply don't appear; new runs start clean. The only landmine is a half-migrated same-run dir, which skip-don't-crash covers.

---

## 12. Skills & Docs

- **Single source of truth** for the agent-facing flow. The current split between the installed `SKILL.md` (legacy) and `reference/feedback.md` (current) must be eliminated.
- **Rewrite `veld-feedback`** to teach exactly the §3 loop: `next --wait` → ITEM/TIMEOUT/ENDED, plus the §7 resolve policy. Keep it minimal; no cursor, no claiming, no sub-agents.
- Per `AGENTS.md` documentation checklist: update `README.md`, `skills/veld/SKILL.md`, `skills/veld/reference/feedback.md`, and any llms docs referencing feedback.

---

## 13. Human-Facing Plumbing (kept — makes "lazy human" work)

- **Pulsing FAB while the agent loop is live** = "someone's listening."
- **Toast / browser notification when the agent replies** = "go look." Load-bearing for a human not staring at the screen.
- The browser keeps its own internal event-polling + cursor. We only remove the cursor from *the agent's* CLI; the event log remains the substrate.

---

## 14. Non-Goals (explicit)

- Parallel / multi-agent orchestration, locks-as-coordination, thread claiming.
- Drawing / annotation suite; interactive control injection; XY-pad.
- Time-based auto-close of threads.
- Client-side sequence cursor for the agent.
- Data migration of pre-existing feedback.

---

## 15. Implementation Phasing

1. **Core loop** — `next --wait` (pure read, derived states, FIFO), `reply`, `ask`, agent `resolve`; delete `listen`/`--after`/claim/release/auto-claim. Skip-don't-crash on old files.
2. **Skill rewrite** — single source of truth, the three-outcome loop, resolve policy.
3. **Tool cull** — delete `draw-overlay/` and the controls/registry/XY-pad subsystems; toolbar down to five actions.
4. **UI fixes** — screenshot freeze-first; picker dark-backdrop cutout.
5. **Docs** — AGENTS.md checklist sweep.

> Note: the overlay toolbar/bubble presentation is being reworked in parallel. Feature set here is stable; exact code locations in the overlay may shift when that lands.

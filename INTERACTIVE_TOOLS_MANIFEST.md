# Interactive Feedback Tools — Design Manifest

> Captured during feedback-system-v2 development. This is the design document
> for the next phase: interactive tools layered on top of the thread system.

---

## Core Insight

Text comments are lossy for visual values. "Make it bigger" requires 3-4 round-trips
to converge on `2.25rem`. Interactive tools let the human express exact values through
direct manipulation — one round-trip, zero ambiguity.

BUT: not everything benefits from a tool. The test for each candidate is:
**"Does text genuinely lose information, or am I building a tool for the sake of it?"**

---

## The Five Primitives

Everything reduces to five interactive primitives. All panels and tools are
compositions of these.

### 1. Smart Slider

Continuous numeric input with three modes:

| Mode | UX | Output | Default? |
|---|---|---|---|
| **Relative** | Scale from current value (0.1x – 3.0x) | Computed value | Yes |
| **Snap** | Tick marks at project tokens / existing page values | Token ref or value | |
| **Free** | Type exact number + unit | Exact value | Power user |

The relative mode is the default because humans think "bigger/smaller", not "2.25rem."
The snap mode shows what already exists in the project so the human stays within the
design system. Free mode is the escape hatch.

Scale ranges per property:
```
font-size       0.25x – 4.0x
line-height     0.5x  – 2.0x
letter-spacing  -2.0x – 3.0x   (can go negative)
padding         0x    – 4.0x   (can go to zero)
border-radius   0x    – 8.0x
gap             0x    – 4.0x
opacity         0 – 1 absolute (no relative mode, already 0-1)
```

### 2. Color / Value Selector

Pick from what exists rather than specifying arbitrary values.

| Mode | UX | Output |
|---|---|---|
| **Token** | Pick from CSS custom properties (--brand-*, --color-*) | `var(--name)` |
| **Page** | Pick from colors computed on this page | Hex or token |
| **Eyedrop** | Click another element to match its color | Reference + resolved value |
| **Free** | Type hex/rgb/hsl | Exact color |

Key insight: a raw HSL color picker is the WRONG tool. The human usually wants to
reference an existing color, not invent a new one. The token palette and eyedropper
cover 90% of cases. Free input is the fallback.

The palette is extracted at runtime:
- Scan `:root` / `html` for CSS custom properties
- Extract unique computed colors from visible elements
- Group semantically where possible

### 3. Box Model Drag

Visual four-sided spacing manipulation.

| Mode | UX | Output |
|---|---|---|
| **Relative** | Drag edges, values scale from current | 4x computed values |
| **Snap** | Edges snap to spacing scale (4, 8, 12, 16, 24, 32) | 4x snapped values |
| **Symmetric** | Shift+drag adjusts opposing sides together | 4x values (pairs locked) |
| **Free** | Click a side, type value | Exact 4x values |

Properties: padding, margin.

### 4. Inline Text Edit

Direct content rewriting on the page.

| Mode | UX | Output |
|---|---|---|
| **Direct** | Double-click text → contenteditable → type | `{ from, to }` string diff |
| **Suggest** | Describe desired change, let agent rewrite | Text comment for agent |

Direct mode is the primary use case. Suggest mode is for when the human knows
the intent but not the exact wording ("make this more action-oriented").

Properties: textContent, placeholder, alt, title.

### 5. Eyedropper (standalone)

"Make this look like that."

| Mode | UX | Output |
|---|---|---|
| **Single property** | Pick element → pick property → applied | Property + reference |
| **Match all** | Pick element → copies all visual styles | Multiple property diffs |

Most design feedback is relational: "same color as the nav", "same padding as the
other cards", "match the button radius." The eyedropper captures this in one action.

---

## Property → Primitive Mapping

Adding a new property is configuration, not new code:

```
font-size       → slider (relative default)
line-height     → slider (relative default)
letter-spacing  → slider (relative default)
opacity         → slider (absolute 0-1, no relative mode)
border-radius   → slider (relative default)
gap             → slider (relative default)
color           → value selector
background      → value selector
border-color    → value selector
padding         → box model drag
margin          → box model drag
textContent     → inline text edit
```

---

## Tool Groupings (Panels)

The primitives compose into contextual panels based on element type:

**Typography Panel** (text elements)
- Font size: slider (relative)
- Line height: slider (relative)
- Letter spacing: slider (relative)
- Color: value selector

**Spacing Panel** (any element)
- Padding: box model drag
- Border radius: slider
- Opacity: slider (absolute)

**Color Panel** (elements with fg/bg color)
- Foreground: value selector
- Background: value selector
- Border: value selector

These panels appear contextually when an element is selected. The overlay detects
the element type (text, container, image, button) and shows relevant panels.

---

## Feedback Event Format

All tools produce the same structured event type:

```json
{
  "type": "style_adjustment",
  "thread_id": "...",
  "selector": "h1.hero-title",
  "component": "HeroSection",
  "source_file": "src/components/Hero.tsx:24",
  "changes": [
    { "property": "font-size", "from": "2rem", "to": "2.4rem", "mode": "relative", "factor": 1.2 },
    { "property": "color", "from": "#ffffff", "to": "var(--text-secondary)", "mode": "token" }
  ],
  "comment": "Softer, lighter feel"
}
```

The event flows through the existing thread system — it's just a richer message type
attached to a thread. The agent receives machine-readable property diffs instead of
(or alongside) text comments.

---

## React/Vue/Framework Connection

### What We CAN Read
- Component name + hierarchy (already doing this via fiber/instance walking)
- Current prop values (from fiber.memoizedProps / Vue instance)
- Current hook state (from fiber.memoizedState)
- Source file hint (from __source in dev builds)

### What We CAN Preview (limited)
- Visual enum props (variant, size, color scheme) — CSS class swap, not real re-render
- Boolean toggles — if they only affect CSS classes

### What We CANNOT Do
- Change props that affect DOM structure (children, conditional renders)
- Change state that triggers side effects
- Add/remove components
- Change event handlers
- Anything requiring actual code execution

### What's Worth Building
1. **Component info badge** — show component name + source file + props on selection
   (read-only, enriches every feedback event)
2. **Visual prop toggles** — for string enum props where preview is CSS class swap
3. **Source file hint** — include in every feedback event so agent skips codebase search

The browser is a viewport, not an editor. The agent writes code. Framework awareness
enriches the feedback context, not the editing capability.

---

## Live Preview Architecture

Every interactive tool follows the same flow:

```
1. Read current value    → getComputedStyle() / element.attributes / fiber.props
2. Present control       → slider / picker / drag handle / contenteditable
3. Live preview          → element.style override (CSS) or textContent mutation
4. Human adjusts         → sees result at 60fps, local to browser
5. Human sends           → one structured event with { property, from, to }
6. Agent receives        → applies to source code → commits
```

Key constraint: step 3 is LOCAL and INSTANT. We only mutate the DOM, never the
source, during preview. The source changes when the agent processes the feedback.
Preview reverts on page reload — until the agent applies it for real.

This is Mode 1 (Adjust → Send). We explicitly chose NOT to do Mode 2 (stream
adjustments to agent in real-time) because:
- Slider drags would flood the agent with hundreds of intermediate values
- Agent can't write code fast enough to keep up with drag events
- HMR reloads during drag would break the interaction
- Git history fills with intermediate garbage commits

The human iterates visually at 60fps. The agent receives one final intent.

---

## What We Decided NOT To Build

| Idea | Why Not |
|---|---|
| Raw HSL color picker | Human should reference existing colors, not invent new ones |
| Box shadow editor | "Add a subtle shadow" is unambiguous enough as text |
| Component prop editor (full) | Framework-specific, fragile, breaks on SSR/hydration |
| Interaction recorder | Captures DOM events, not intent — gap still needs text |
| Responsive breakpoint tester | Human can resize browser; "breaks at ~800px" is precise enough |
| Drag-to-reorder layout | "Move pricing card first" is one sentence, drag system is massive effort |
| Animation curve editor | Too specialized, niche need |
| Link href editor | "Should go to /pricing" is unambiguous |
| Form field editor | "Make email required" is unambiguous |
| Transform/rotate editor | Rarely requested in feedback |
| Streaming adjustments to agent (Mode 2) | Floods agent, HMR conflicts, garbage commits |

General rule: if the human can express it unambiguously in one sentence, a text
comment in a thread is the right tool. Interactive tools are for values where text
is genuinely lossy.

---

## Build Priority

Phase 1 (highest impact, lowest complexity):
1. **Inline text edit** — zero UI to build, just contenteditable + diff capture
2. **Relative sliders for typography** — font-size, line-height, letter-spacing
3. **Component info badge** — read-only framework context on every selection

Phase 2:
4. **Color value selector** — token palette + page colors + eyedropper
5. **Box model drag** — visual padding/margin manipulation
6. **Snap mode for sliders** — detect project tokens, show as tick marks

Phase 3:
7. **Eyedropper (standalone)** — "match all styles from that element"
8. **Visual prop toggles** — enum prop preview via CSS class swap
9. **Source file hints** — include __source in feedback events

Each phase ships independently on top of the thread system. The thread is the
transport — interactive adjustments are just a richer message type within threads.

---

## Open Questions

- Should interactive adjustments auto-create a thread, or attach to an existing one?
- Should the human be able to batch multiple adjustments (e.g., change 3 properties
  across 2 elements) into a single send?
- How do we handle undo? Reset individual properties, or reset all adjustments?
- Should the agent be able to SEND interactive controls back? ("Pick which variant
  you prefer: [A] [B] [C]" as a structured message in the thread)

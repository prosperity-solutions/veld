---
name: veld-feedback
description: Collaborate with humans through Veld's in-browser feedback overlay. Use when the user asks you to get feedback on UI changes, request a human review, show your work to someone, or when you need visual verification of frontend changes. Also use when iterating on design based on reviewer comments.
metadata:
  author: prosperity-solutions
  version: "1.0.0"
---

# Human-in-the-Loop Feedback with Veld

Veld injects a feedback overlay into every page served through it. This lets you pause, ask a human to review your work in the browser, and receive structured feedback — element-level comments, screenshots, component traces — that you can act on programmatically.

## When to Use This

- After making visual/UI changes that need human eyes
- When you're unsure if a design looks right
- When the user asks you to "show" them something
- When iterating on frontend work and want fast feedback loops
- After deploying a fix to verify it looks correct

## The Feedback Loop

```
You make changes
    ↓
veld feedback --wait --name <run>
    ↓
Human sees "Feedback Requested" modal in browser
    ↓
Human reviews, leaves comments, takes screenshots
    ↓
Human clicks "Submit Feedback" (or "All Good")
    ↓
CLI receives structured feedback
    ↓
You read the feedback and make changes
    ↓
Repeat
```

## Step-by-Step

### 1. Make sure a Veld environment is running

```bash
# Check if there's already a run
veld runs

# If not, start one
veld start frontend:local --name dev
```

### 2. Request feedback

```bash
veld feedback --wait --name dev
```

This blocks until the human submits feedback. While waiting:
- The browser overlay shows a "Feedback Requested" modal to the human
- The page hard-reloads so the reviewer sees your latest changes
- A browser notification fires (if permitted)
- The FAB pulses green to attract attention

### 3. Read the output

The command prints structured feedback when the human submits:

```
Feedback for run 'dev' (3 comments):

  #1  "The header text is too small on mobile"
     Element: header > h1.title
     Components: App > Layout > Header
     Page: /dashboard
     Viewport: 1440×900

  #2  "This button color doesn't match the design"
     Element: div.actions > button.primary
     Selected text: "Submit"
     Screenshot: /path/to/screenshot.png
     Page: /dashboard

  #3  "Footer links are broken"
     Page: /about
```

### 4. Act on the feedback

Read each comment and make the requested changes. Key fields to use:

| Field | What it tells you |
|-------|-------------------|
| `comment` | What the human wants changed |
| `element_selector` | CSS selector of the element — find it in your code |
| `selected_text` | Specific text the human highlighted |
| `component_trace` | React/Vue component hierarchy — find the right component file |
| `screenshot` | Path to a PNG — read it to see exactly what the human sees |
| `page_url` | Which page/route the comment is about |
| `viewport_width/height` | Screen dimensions — check if it's a responsive issue |

### 5. Request another round

After making changes, request feedback again:

```bash
veld feedback --wait --name dev
```

The page will hard-reload, showing the human your updates. Repeat until they click "All Good" (you'll see "Reviewer approved — all good, no feedback.").

## Handling Outcomes

The `--wait` flag produces three possible outcomes:

1. **Feedback submitted** — Comments are printed. Read them and make changes.
2. **Approved** — Output: `Reviewer approved — all good, no feedback.` — You're done, move on.
3. **Cancelled** — Output: `Feedback cancelled by reviewer.` — The human doesn't want to review right now. Don't request again immediately.

## Reading Past Feedback

```bash
# Latest batch
veld feedback --name dev

# All historical batches
veld feedback --history --name dev

# Machine-readable
veld feedback --json --name dev
```

## JSON Output for Programmatic Use

```bash
veld feedback --json --name dev
```

Returns structured JSON with all fields. Useful when you need to parse feedback programmatically rather than reading the human-formatted output.

## Best Practices

### Do request feedback when:
- You've made visual changes and want confirmation
- You're unsure about a design decision
- The user explicitly asked you to show them the result
- You've fixed a reported visual bug and want verification

### Don't request feedback when:
- You're making backend-only changes with no visual impact
- The user hasn't asked for a review
- You just requested feedback and the reviewer cancelled — wait for them to be ready
- You're in the middle of a multi-step change — finish first, then request once

### Make it easy for the reviewer:
- Tell the human what you changed before requesting feedback (they'll know what to look at)
- If you changed a specific page, mention which URL to check
- Keep change sets small and focused — one concern per feedback round

### When reading component traces:
- The trace shows the React/Vue component hierarchy from root to leaf: `App > Layout > Sidebar > NavItem`
- Use this to locate the right source file — search for the component name
- The deepest component in the trace is usually the one to edit

### When reading screenshots:
- Screenshots are PNG files on disk — you can read them to see what the human saw
- They show the exact viewport state at the time of capture
- Useful for responsive issues where the problem might not be obvious from code alone

## Example Workflow

```
User: "Make the dashboard header match the new brand colors"

You:
1. Find the header component using the codebase
2. Update the colors
3. Tell the user what you changed
4. Run: veld feedback --wait --name dev
5. Human reviews in browser, leaves a comment:
   "The gradient looks good but the text is hard to read against it"
   Element: header > h1.page-title
   Components: App > DashboardLayout > Header
6. You adjust the text color/contrast
7. Run: veld feedback --wait --name dev
8. Human clicks "All Good"
9. Done.
```

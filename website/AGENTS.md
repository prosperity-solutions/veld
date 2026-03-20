# Agents Guide — veld website

This directory contains the veld marketing website: three static HTML pages served via nginx in Docker.

## Three-Page Architecture

| Page | Route | File | Purpose |
|------|-------|------|---------|
| Experience | `/` | `index.html` | Cinematic scroll homepage. Primary landing page for visitors. |
| Humans | `/humans` | `humans.html` | Traditional documentation and marketing page for human readers. |
| Agents | `/agents` | `agents.html` | Structured, agent-friendly page. Primary audience: LLMs and developers who think in terminals. |

All three pages share a navigation bar: **experience · humans · agents**.

## Dual Content Maintenance

The website has **two audiences** — humans and LLMs. When you change content, update both:

| Human/Agent version | LLM version |
|---------------------|-------------|
| `index.html` (experience homepage) | `llms.txt` (structured markdown index) |
| `humans.html` (styled docs for humans) | `llms-full.txt` (full content inlined) |
| `agents.html` (structured agentic view) | |

### Rules

1. **Any content change to `agents.html` must be reflected in `llms-full.txt`** — features, examples, config snippets, install commands, CLI reference.
2. **If you add or rename a documentation page**, update the links in `llms.txt`.
3. **`llms-full.txt` is the single-request version** — it contains all content inlined. Keep it comprehensive but concise. No HTML, no styling, just clean markdown.
4. **`llms.txt` is the index** — it links to raw markdown files on GitHub and lists the site pages. Only update it when doc structure changes.
5. **Keep the veld.json example in `llms-full.txt` consistent** with the one in `agents.html`. Both should reflect the same config structure.

### Correctness constraints

- `command` type steps do NOT get `${veld.port}` allocated — only `start_server` does.
- `start_server` outputs are objects (synthetic templates); `command` outputs are arrays (captured from `$VELD_OUTPUT_FILE` or legacy `VELD_OUTPUT` stdout).
- The domain is `veld.oss.life.li`, not `veld.dev`.
- URL templates use `{variable}` (single braces); commands/env use `${variable}`.
- The install URL is `https://veld.oss.life.li/get` (nginx redirects to GitHub).

## Visual Identity

### Colors

| Token | Value | Usage |
|-------|-------|-------|
| Background | `#0A0A0B` | Page background (all pages) |
| Text | `#E8E4DF` | Primary body text |
| Accent | `#C4F56A` | Highlights, CTAs, active nav |
| Links | `#5B8DEF` | Hyperlinks |

### Typography

| Page | Fonts |
|------|-------|
| Experience (`/`) | Playfair Display (serif narrative), Space Grotesk (headings), system monospace |
| Humans (`/humans`) | Space Grotesk (headings), Inter (body, falls back to system sans), JetBrains Mono (code) |
| Agents (`/agents`) | JetBrains Mono (body), system sans (nav) |

## Convention Files

- **`llms.txt`** — Index file following the llms.txt convention. Lists project metadata and links to documentation. Served at `/llms.txt` and discoverable via `/.well-known/llms.txt` (redirect).
- **`llms-full.txt`** — Full content version. All documentation inlined into a single markdown file for single-request consumption by LLMs.
- Both are served with `Content-Type: text/markdown` and `Cache-Control: public, max-age=3600`.
- The `/` route includes `Link` response headers pointing to both files for automated agent discovery.

## File Structure

```
website/
├── index.html         # Experience page (cinematic homepage, served at /)
├── humans.html        # Humans page (docs + marketing, served at /humans)
├── agents.html        # Agents page (structured agentic view, served at /agents)
├── og.png             # Open Graph image (1200x630)
├── favicon.svg        # Favicon
├── logo.svg           # Logo
├── logo-wordmark.svg  # Logo wordmark
├── fonts/             # Self-hosted web fonts (woff2)
├── llms.txt           # LLM index (markdown, links to docs)
├── llms-full.txt      # LLM full content (all docs inlined)
├── nginx.conf         # nginx config (routes, headers, redirects)
├── robots.txt         # Robots/crawler directives
├── Dockerfile         # nginx:alpine container
└── AGENTS.md          # This file
```

## Local Development

The root `veld.json` includes a `website` node that serves this directory locally:

```sh
veld start website:local --name dev
```

This starts a Python HTTP server and gives you an HTTPS URL like `https://website.dev.veld.localhost`. Use the feedback overlay to collaborate on design changes with AI agents.

## Production Serving

- Static files served by nginx:alpine
- `/` → `index.html` (with `Link` headers for llms.txt discovery)
- `/humans` → `humans.html`
- `/agents` → `agents.html`
- `/get` → 302 redirect to GitHub install script
- `/schema/` → rewrite redirect to GitHub raw schema files
- `/llms.txt` and `/llms-full.txt` served as `text/markdown`
- `/.well-known/llms.txt` → 301 redirect to `/llms.txt`
- `/robots.txt` → crawler directives
- `/health` → 200 OK (for container health checks)
- All other paths → SPA fallback to `index.html`

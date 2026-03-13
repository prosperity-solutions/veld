# Agents Guide — veld website

This directory contains the veld marketing website: a single static HTML page served via nginx in Docker.

## Dual Content Maintenance

The website has **two audiences** — humans and LLMs. When you change content, update both:

| Human version | LLM version |
|---------------|-------------|
| `index.html` (styled HTML with animations) | `llms.txt` (structured markdown index) |
| | `llms-full.txt` (full content inlined) |

### Rules

1. **Any content change to `index.html` must be reflected in `llms-full.txt`** — features, examples, config snippets, install commands, CLI reference.
2. **If you add or rename a documentation page**, update the links in `llms.txt`.
3. **`llms-full.txt` is the single-request version** — it contains all content inlined. Keep it comprehensive but concise. No HTML, no styling, just clean markdown.
4. **`llms.txt` is the index** — it links to raw markdown files on GitHub. Only update it when doc structure changes.
5. **Keep the veld.json example in `llms-full.txt` consistent** with the one in `index.html`. Both should reflect the same config structure.

### Correctness constraints

- `command` type steps do NOT get `${veld.port}` allocated — only `start_server` does.
- `start_server` outputs are objects (synthetic templates); `command` outputs are arrays (captured from VELD_OUTPUT).
- The domain is `veld.oss.life.li`, not `veld.dev`.
- URL templates use `{variable}` (single braces); commands/env use `${variable}`.
- The install URL is `https://veld.oss.life.li/get` (nginx redirects to GitHub).

## File Structure

```
website/
├── index.html         # The website (HTML + inline CSS + JS)
├── og.png             # Open Graph image (1200x630)
├── llms.txt           # LLM index (markdown, links to docs)
├── llms-full.txt      # LLM full content (all docs inlined)
├── nginx.conf         # nginx config (routes, headers, redirects)
├── Dockerfile         # nginx:alpine container
└── AGENTS.md          # This file
```

## Serving

- Static files served by nginx:alpine
- `/get` → 302 redirect to GitHub install script
- `/llms.txt` and `/llms-full.txt` served as `text/markdown`
- `/health` → 200 OK (for container health checks)
- All other paths → SPA fallback to `index.html`

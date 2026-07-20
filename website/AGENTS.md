# Agents Guide — veld website

This directory contains the veld marketing website: **one static HTML page**
served via nginx in Docker. It used to be three pages (experience / humans /
agents); those were removed in favor of a single, boring, self-explanatory
landing page that's easy to maintain. Let the tool speak for itself.

## One-page architecture

| Surface | Route | File | Purpose |
|---------|-------|------|---------|
| Landing page | `/` | `index.html` | The entire human-facing site. Self-contained: inline CSS, embedded wordmark SVG, data-URI favicon, self-hosted fonts. |
| LLM index | `/llms.txt` | `llms.txt` | llms.txt-convention index; links to GitHub docs. |
| LLM full docs | `/llms-full.txt` | `llms-full.txt` | All docs inlined into one markdown file for single-request LLM consumption. |

`index.html` sections: hero + install, features, components + architecture
diagram, CLI reference, install, sharing, agents/skill, docs, kudos, footer.

## Dual content maintenance

The website has **two audiences** — humans (`index.html`) and LLMs
(`llms.txt` / `llms-full.txt`). When you change content, update both:

1. **Any factual/content change to `index.html`** (features, CLI, config, install
   command, architecture) must be reflected in `llms-full.txt`.
2. **`llms-full.txt` is the single-request version** — all content inlined, clean
   markdown, no HTML. Comprehensive but concise.
3. **`llms.txt` is the index** — links to raw markdown on GitHub plus the site.
   Only update it when doc structure changes.
4. Keep any `veld.json` example consistent across `index.html` and
   `llms-full.txt`.

### Correctness constraints

- `command` type steps do NOT get `${veld.port}` allocated — only `start_server` does.
- `start_server` outputs are objects; `command` outputs are arrays.
- The domain is `veld.oss.life.li`, not `veld.dev`.
- URL templates use `{variable}` (single braces); commands/env use `${variable}`.
- The install URL is `https://veld.oss.life.li/get` (nginx redirects to GitHub).

## Visual identity

Follows [../docs/branding.md](../docs/branding.md) — the marketing palette:

The page ships **both a dark and a light theme** (default dark; follows the OS via
`prefers-color-scheme`, with a three-state auto/light/dark toggle). Colors are CSS
custom properties defined per theme — never hard-code a hex in a rule. Dark values
shown; light overrides live in `:root[data-theme="light"]` + the
`@media (prefers-color-scheme: light)` block. See [../docs/branding.md](../docs/branding.md).

| Token | Dark value | Usage |
|-------|------------|-------|
| `--bg` | `#0A0A0B` | Page background |
| `--text` | `#E8E4DF` | Primary body text |
| `--accent` | `#C4F56A` | Brand lime — **subtle fills / tints only** (not legible as text on white) |
| `--accent-ink` | `#C4F56A` (dark) / `#3E7A10` (light) | Accent as **text or a mark**: headline accent, wordmark dot, primary button bg |
| `--accent-ink-text` | `#0A0A0B` (dark) / `#FFFFFF` (light) | Text on top of `--accent-ink` (e.g. the primary button label) |
| `--blue` | `#5B8DEF` (dark) / `#2E5BD0` (light) | Links |

**Two-token accent rule:** the wordmark dot, the headline accent, and the primary
button must all use `--accent-ink` (same green per theme) — never `--accent`, which
is lime and unreadable on light. Keep them in sync.

Typography is **JetBrains Mono** (self-hosted, `fonts/`) for headings, code, and
UI chrome, with a system sans-serif for body prose. The wordmark is the embedded
`logo-wordmark.svg` path data, colored via CSS (letters `--text`, trailing dot
`--accent-ink`) — the dot is the **first** `<path>` in that SVG's source order.

**Command blocks:** any `<pre>` whose visible text includes `#` comments or `=>`
output lines must carry a `data-copy="…"` attribute with the clean command(s)
only — the copy button copies `data-copy` when present, else the block's full
`innerText` (which would include the comments/output).

**CSP note:** the page is self-contained (no external requests) but uses inline
`<style>` and inline `<script>` for the theme toggle + copy buttons, so it needs
`'unsafe-inline'` (or nonces/hashes) under a strict CSP. nginx sets no CSP header
today; don't add a `default-src 'self'` without allowing inline, or the theme and
copy buttons break.

## Convention files

- **`llms.txt`** — index; served at `/llms.txt`, discoverable via
  `/.well-known/llms.txt` (301 redirect).
- **`llms-full.txt`** — full inlined content.
- Both served as `text/markdown` with `Cache-Control: public, max-age=3600`.
- The `/` route sends `Link` response headers pointing to both files for agent
  discovery.

## File structure

```
website/
├── index.html         # The whole site (served at /)
├── og.png             # Open Graph image (1200x630)
├── favicon.svg        # Favicon (also inlined as data-URI in index.html)
├── logo.svg           # Icon logo
├── logo-wordmark.svg  # Wordmark (source for the inlined nav SVG)
├── fonts/             # Self-hosted JetBrains Mono (woff2)
├── llms.txt           # LLM index
├── llms-full.txt      # LLM full content
├── nginx.conf         # nginx config (routes, headers, redirects)
├── robots.txt         # Crawler directives
├── Dockerfile         # nginx:alpine container
└── AGENTS.md          # This file
```

## Local development

The root `veld.json` serves this directory locally via browser-sync:

```sh
veld start website:local --name dev
```

This gives an HTTPS URL like `https://website.dev.veld.localhost`. Use the
feedback overlay to collaborate on design changes with a human reviewer.

## Production serving

Static files served by `nginx:alpine`:

- `/` → `index.html` (with `Link` headers for llms discovery)
- `/humans` and `/agents` → 301 redirect to `/` (retired pages)
- `/get` → 302 redirect to the GitHub install script
- `/schema/` → rewrite redirect to GitHub raw schema files
- `/llms.txt` and `/llms-full.txt` served as `text/markdown`
- `/.well-known/llms.txt` → 301 redirect to `/llms.txt`
- `/robots.txt` → crawler directives
- `/health` → 200 OK (container health check)
- All other paths → fallback to `index.html`

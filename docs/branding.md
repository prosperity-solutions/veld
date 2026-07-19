# Veld branding — for every user-facing surface

This is the canonical reference for how anything Veld shows to a human in a
browser must look. It exists so that new HTML surfaces (a gateway page, a
daemon UI, an error page, an overlay) come out branded by default instead of
falling back to unstyled system-default pages.

**The rule:** any HTML page a Veld binary serves to a browser carries the
Veld brand — the wordmark, the dark token palette below, and self-contained
assets (inline CSS, embedded SVG, data-URI favicon; no external requests, so
pages render under any CSP). No page ships looking like a bare `<h1>` on
white.

**Scope:** the rule covers pages a human is meant to read in a browser —
index, login, 404s, viewer-facing error pages (dead tunnel, upstream
unresponsive or timed out — whether the request was a plain fetch or a
WebSocket upgrade attempt). Machine-facing responses stay plain text by
design: API errors (Bearer-gated registration), health probe bodies,
abuse-path guards (405/413/oversized form), and belt-and-braces guards on
practically-unreachable paths (e.g. a password-mode slug without a
password, a non-upgradable client connection at splice time). Responses an
origin app produces and the gateway merely proxies are the app's own.

## Brand assets

| Asset | Canonical source | Notes |
|-------|------------------|-------|
| Icon logo (`V.`) | `logo.svg` (repo root) | White `V` + accent-green dot, 32×32 viewBox |
| Wordmark (`veld.`) | `crates/veld-daemon/assets/management-ui.html` (header SVG) and `crates/veld-gateway/src/pages.rs` (`WORDMARK_SVG`) | Letters take `var(--text)`, the final dot path takes `var(--accent)` |
| Favicon | data-URI SVG in `website/index.html` and `crates/veld-gateway/src/pages.rs` | Rounded dark square, white `V`, accent dot |

The wordmark's trailing dot is always the accent green — that is the brand's
signature detail. When embedding the wordmark, color it via CSS
(`.wordmark path{fill:var(--text)} .wordmark path:last-child{fill:var(--accent)}`)
rather than hard-coded fills, so it follows the palette.

## Product UI palette (dark)

Used by the daemon management UI, the gateway pages, and any future product
surface. Tokens (from `crates/veld-daemon/assets/management-ui.html`):

```css
:root{
  --bg:#0f1117;--surface:#181a24;--surface2:#1f2233;--border:#2a2d3e;
  --text:#e0e0e6;--text2:#8b8fa3;
  --accent:#C4F56A;--accent-bg:rgba(196,245,106,.12);
  --blue:#6c8cff;
  --green:#3dd68c;--green-bg:rgba(61,214,140,.1);
  --yellow:#f0c040;--yellow-bg:rgba(240,192,64,.1);
  --red:#f06060;--red-bg:rgba(240,96,96,.1);
  --dim:#555870;
}
```

Conventions: system-ui font stack, 8–12px border radii, `--surface` cards on
the `--bg` page with 1px `--border`, links in `--blue`, primary buttons in
`--accent` with dark text, errors in `--red`. Header pattern:
`wordmark / <subtitle>` (see the management UI and gateway pages).

## Marketing palette (website)

The website (`website/`) uses its own darker, editorial variant — same accent:

```css
:root{
  --bg:#0A0A0B;--text:#E8E4DF;--accent:#C4F56A;--blue:#5B8DEF;
  --muted:#4A4A50;--red:#ef4444;--code-bg:#141416;
}
```

with JetBrains Mono / Playfair Display / Space Grotesk. Marketing pages follow
`website/AGENTS.md`; product surfaces do NOT adopt the marketing fonts.

## Checklist for a new user-facing page

- [ ] Wordmark embedded (SVG, CSS-colored), accent dot green
- [ ] Product palette tokens above (dark), not ad-hoc colors
- [ ] Self-contained: inline CSS, no external fonts/scripts/images
- [ ] `<meta name="viewport">` and (for non-indexable surfaces) `<meta name="robots" content="noindex">`
- [ ] Data-URI favicon
- [ ] No sensitive/enumerable data on anonymous pages (share names, hostnames, counts).
      Client-side, tab-scoped reveals are the one vetted exception: a page may
      swap its copy from browser-local state the viewer's own tab minted
      earlier (see `SHARE_SEEN_KEY` in `crates/veld-gateway/src/pages.rs`),
      as long as the served bytes stay identical for every viewer.
- [ ] Reuse an existing shell where one exists (`crates/veld-gateway/src/pages.rs::shell` for gateway pages) instead of a new bespoke page

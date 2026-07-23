# Veld branding — for every user-facing surface

This is the canonical reference for how anything Veld shows to a human in a
browser must look. It exists so that new HTML surfaces (a gateway page, a
daemon UI, an error page, an overlay) come out branded by default instead of
falling back to unstyled system-default pages.

**The rule:** any HTML page a Veld binary serves to a browser carries the
Veld brand — the wordmark, the dark token palette below, and self-contained
assets (inline CSS, inline JS where needed, embedded SVG, data-URI favicon; no
external requests, so nothing cross-origin is left to block). No page ships
looking like a bare `<h1>` on white.

Note: "self-contained" removes *external-host* CSP failures, not *inline* ones —
inline `<style>`/`<script>` still require `'unsafe-inline'` (or nonces/hashes)
under a strict CSP. A page that adds a strict `Content-Security-Policy` must
account for its own inline CSS/JS.

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

## Desktop theme (management UI v2 / Veld Desktop)

The `/ide` management UI (`crates/veld-daemon/ui/`, wrapped by the Electron
shell in `desktop/`) is the sanctioned second product theme. It follows the
Veld Desktop design handoff rather than the classic product palette above:
dark **and** light themes, a green accent, **Inter** for UI text, and
**JetBrains Mono** for branches/URLs/aliases/terminal content (an explicit
exception to the "no marketing fonts on product surfaces" rule below — both
fonts are bundled into the page, not fetched).

```css
/* dark — default */
:root{
  --bg:#0d0e10;--panel:#141619;--panel2:#1a1d21;--elev:#22262c;
  --border:#2a2e35;--border2:#363b43;--text:#e7e9ec;--muted:#98a0a9;--faint:#666d76;
  --live:oklch(0.74 0.14 158);--live-ink:#04140b;--live-bg:rgba(63,191,127,.14);
  --warn:oklch(0.82 0.13 82);--warn-bg:rgba(230,180,60,.14);
  --danger:oklch(0.68 0.17 22);--danger-bg:rgba(224,90,80,.14);
  --accent:oklch(0.74 0.14 158);
}
/* light — body[data-theme="light"]; see crates/veld-daemon/ui/src/styles.css */
```

The structural rules are unchanged for this theme: embedded wordmark with the
accent dot (here the accent green), fully self-contained single-file bundle
(fonts inlined), viewport + noindex metas, data-URI favicon. The classic
palette above remains the default for every *other* product surface (gateway
pages, v1 dashboard, error pages).

## Marketing palette (website)

The website (`website/`, a single self-contained `index.html`) uses its own
editorial variant of the palette — same lime accent — and ships **both a dark
and a light theme**. Dark is the default; the page also follows the visitor's
OS via `prefers-color-scheme` and offers a three-state toggle
(auto → light → dark, remembered in `localStorage`).

```css
/* dark — default / forced-dark */
:root{
  --bg:#0A0A0B;--surface:#141416;--surface2:#1a1a1d;--border:#26262b;
  --text:#E8E4DF;--text2:#9a9a93;
  --accent:#C4F56A;          /* brand lime — subtle fills / tints */
  --accent-ink:#C4F56A;      /* accent as text/mark + primary button */
  --accent-ink-text:#0A0A0B; /* text on top of --accent-ink */
  --blue:#5B8DEF;--muted:#7c7c86;--code-bg:#141416;
}
/* light — forced-light or system-light */
:root[data-theme="light"]{
  --bg:#FBFBF8;--surface:#FFFFFF;--surface2:#F2F2ED;--border:#E3E3DC;
  --text:#17171A;--text2:#56565d;
  --accent:#C4F56A;          /* lime as a subtle tint only */
  --accent-ink:#3E7A10;      /* darker green so accent text stays legible on white */
  --accent-ink-text:#FFFFFF; /* white text on the dark-green button */
  --blue:#2E5BD0;--muted:#74747e;--code-bg:#F5F5F0;
}
```

**Two-token accent rule:** lime (`--accent`) is only legible on dark. Use it for
subtle fills and tints; use `--accent-ink` for anything that must read as
green *text or a mark* (headline accent, the wordmark dot, the primary button)
so it stays readable in light mode. Keep those in sync — the wordmark dot,
title accent, and primary button must all be the same green per theme.

Typography is **JetBrains Mono** (self-hosted in `website/fonts/`) for headings,
code, and UI chrome, with a system sans-serif for body prose. (The earlier
Playfair Display / Space Grotesk experiment is gone.) Marketing pages follow
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

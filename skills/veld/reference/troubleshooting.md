# Troubleshooting

## Content-Security-Policy

Veld serves through a reverse proxy on a different hostname and port than the app's dev server. The app's CSP must allow this. Check for:

- **`connect-src`**: Must include the veld proxy origin for WebSocket (HMR) and fetch. Don't hardcode the dev server port — veld allocates ports dynamically (19000–29999). Use wildcards:
  ```
  connect-src 'self' ws://*:* wss://*:* https://*.localhost
  ```
- **`img-src`**: Must include `blob:` if the app uses blob URLs (e.g. screenshots, image previews):
  ```
  img-src 'self' data: blob:
  ```
- **`script-src`**: Must allow `'unsafe-inline'` and `'unsafe-eval'` in development (required by most dev servers anyway).

## Overlay not working with automatic injection

If the veld toolbar doesn't appear or conflicts with the framework, set `"inject": false` on the node in `veld.json` and add the scripts manually:

```html
<script src="/__veld__/feedback/script.js"></script>
<script src="/__veld__/api/client-log.js" data-veld-levels="log,warn,error"></script>
```

For Next.js, use `next/script` with `strategy="afterInteractive"`.

Everything else still works — `/__veld__/*` API routes, all CLI commands, the full overlay UI. Only the automatic HTML injection is disabled.

**If you need this workaround, [open a GitHub issue](https://github.com/prosperity-solutions/veld/issues)** so we can fix automatic injection for your setup.

## Browser says the certificate is expired (`ERR_CERT_DATE_INVALID`)

Caddy issues veld's HTTPS certificates from a local CA and renews them itself.
When that renewal stops — its certificate maintenance is one goroutine, and a
Caddy whose maintenance has stalled keeps answering everything else normally —
the leaf expires and browsers refuse every veld URL.

Veld now notices this by itself: the helper reads the served certificate once a
minute and restarts Caddy when renewal is overdue, which is what renews it (a
config *reload* cannot — Caddy will not re-examine a certificate already in its
cache). So the first move is to look, not to fix:

```sh
veld doctor          # `Certificate:` row, and the HTTPS certificate check
```

- `valid, expires in …` — nothing to do.
- `EXPIRED …` — the watchdog restarts Caddy to try to renew it: two bad probes a
  minute apart, then a restart, up to three times before it stops trying (a fault
  a new process cannot fix is not worth dropping every live connection for, over
  and over). Run `veld doctor`
  again. If it stays expired, Caddy is failing to issue and the reason is in
  `~/.local/lib/veld/caddy-data/caddy.log` — the path doctor prints in its
  Installation block, which is the copy to trust if these ever disagree.
- `no TLS answer (…)` or `unreadable (…)` — Caddy is not managing to issue a
  certificate at all, and this is the one state veld will **not** try to fix by
  itself (a restart is not the answer, and restarting on a probe that learned
  nothing would be guessing). The reason is in that same `caddy.log`; a failing
  `tls.obtain` line names it.
- `NOT VALID for another …` — this machine's clock is behind the certificate's
  start date. Fix the clock; reissuing under a wrong clock just produces another
  certificate the browser rejects, so veld does not restart Caddy for this.
- `not trusted` on the `CA:` row instead is a different problem — the authority
  was never trusted on this machine. Re-run `veld setup <mode>`.

The row names **one** hostname (`veld.localhost`). Every run URL has a
certificate of its own, so a green row is not a verdict on all of them — the
helper's watchdog checks every hostname it serves once a minute, and that is what
acts.

Clicking through the browser warning works but is not a fix: the certificate is
what the shared and injected surfaces are served over too.

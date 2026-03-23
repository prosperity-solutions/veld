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

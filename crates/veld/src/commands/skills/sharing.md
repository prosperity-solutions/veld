# Sharing an environment

Share a running environment with a colleague so they open the **same** URLs on
their own machine, over an encrypted P2P tunnel (iroh: QUIC + NAT hole-punching
+ n0 relay fallback). No accounts, no Veld-hosted server.

**Opt-in is required, and it is per PORT.** `share` is a field on a port entry —
that is where exposure happens, and it is the only place consent is granted.
`veld share` errors on anything that hasn't opted in, listing candidates as
`node:variant#port`.

```jsonc
"ports": {
  "http":     { "port": "auto", "protocol": "http", "share": { "expose": ["peer", "web"] } },
  "admin":    { "port": "auto", "protocol": "http" },                     // absent → NEVER shared
  "postgres": { "port": 5432,   "protocol": "tcp",  "share": { "expose": ["peer"] } }
}
```

A node/variant-level `"share": { "expose": ["peer"] }` still works and is
**defined as shorthand for the PRIMARY port's policy** — it never spreads to the
node's other ports, so every pre-existing config means exactly what it meant. A
port's own `share` replaces the shorthand for that port (it does not merge), and
absent is always "not shared": nothing anywhere widens a port that declared none.
So **do not** add a node-level `share` when the user asks to share one port of a
multi-port node — put it on that port. And a node with no primary (all-`tcp`, or
`"ports": null`) has nowhere to fold the shorthand into, so it grants nothing.

**`"expose": ["web"]` requires `"protocol": "http"`** — lint rule
`web-share-needs-http` (error). The gateway speaks HTTP/1.1 and a browser cannot
speak a raw protocol through it; this is what `web` *means*, not a gap to be
lifted later. A database goes to `peer`.

**Raw `tcp` sharing is peer-only, and the joiner's port number is different from
yours.** A `tcp` port opted into `peer` is reproduced on the joining machine as a
bare local TCP port with no Caddy route, so nothing preserves the original number.
`veld join` prints these separately from URLs as `host:port  (tcp)`, and `--json`
puts them in `addresses` (URLs stay in `urls`). Tell the user to use the printed
address — never the origin's port from `veld.json`. There is no `udp`.
A joiner on an older veld refuses the *whole* join when the manifest carries a
tcp endpoint (its `url` is absent and the old wire format required it) — that is
deliberate fail-closed behaviour, not a bug; both sides must upgrade.

```sh
veld share my-feature                       # print a join URL to send (plus a veld join command)
veld share my-feature --node frontend       # narrow to specific nodes (repeatable; never widens consent)
veld share my-feature --ttl 3600            # TTL in seconds, one share; no upper
                                            # bound, floored at 60s
                                            # Defaults: 14400 peer / 7200 web, from
                                            # sharing.peerTtlMinutes / .webTtlMinutes
                                            # (veld settings), overridable per project
                                            # via sharing.peer_ttl_minutes in veld.json
veld share my-feature --approve first        # first|manual|auto (default: manual, or first with --json)
veld join veldshare_… --label alice         # terminal join by ticket; blocks until the host approves
veld shares                                  # list active shares, joins, pending requests
veld approve <REQ_ID>                        # resolve a pending join request
veld deny <REQ_ID>
veld unshare [SHARE_ID]                      # stop hosting a share (id optional → sole active share)
veld leave [JOIN_ID]                         # disconnect from a joined share (id optional → sole active join)
```

`veld share` prints a **join URL** as the primary way to share:
`https://veld.localhost/join#<ticket>` (or `:18443` in unprivileged mode), plus a
`veld join <ticket>` command as an alternative; `--json` adds a `join_url` field.
The recipient **opens the URL in their browser** — it loads their own Veld
dashboard, which connects, waits for host approval, then shows the shared URLs as
clickable links. The ticket is short and constant-size regardless of how many URLs
the run exposes — the manifest is sent over the tunnel after approval, not embedded
in the ticket. You can also share from the **dashboard**: each running run's card
has a **Share** button (which also copies the join link to your clipboard); once
shared it shows **Copy link** / **Copy command** buttons, a live joiner count, an
**auto-accept** toggle, and **Stop sharing**, with pending join requests
(Approve/Deny) and joined shares in a panel.

Two gates protect a share: a capability token in the ticket, plus host approval.
Approval modes: `manual` (host approves each join via the dashboard — which opens
automatically — or `veld approve`), `first` (auto-approve + pin the first
token-valid joiner, reject the rest), `auto` (approve any token-valid joiner).
Traffic is end-to-end encrypted; a relay only forwards sealed bytes and never
sees URLs or content. Relay selection is a config compliance control and must be
opted into explicitly (no implicit default): set `sharing.relays` to `"public"`
or an array of self-hosted relay URLs, else `veld share` is refused. **`"public"`
(n0's relays) is dev/testing only** — rate-limited, best-effort, no guarantees;
production or high-volume sharing should self-host relays (n0's fair-use guidance,
not a license limit; iroh is MIT/Apache-2.0). Config wins
over the legacy `VELD_SHARE_RELAY` env var (read from the daemon's env, not your
shell; not an enforceable floor). The daemon binds one iroh endpoint per relay
policy on demand, so shares on different relays run concurrently. A self-hosted
relay can require an auth token: write the relay as `{ "url": ..., "token": ... }`
where `token` is a literal string or `{ "env": ... }` / `{ "file": ... }` /
`{ "argv": [...] }` / `{ "shell": "..." }` (resolved on the daemon at share time; keep secrets out of
`veld.json` with the non-literal forms). A joiner auto-confines to the relay(s) in
the ticket (a custom-relay share is never joined over public relays); to reach a
token-gated relay it is prompted for the token (browser overlay / `veld join`
terminal; cached per relay in the veld database (`<data_dir>/veld/veld.db`, 0600); wrong
token re-prompts; `--json` returns `needs_relay_token`). The token can also come
from `VELD_SHARE_RELAY` + `VELD_SHARE_RELAY_TOKEN` (sent only when the URL matches
the ticket's relay), or the host sets `sharing.dangerouslyEmbedRelayTokensInTicket:
true` to embed it in the ticket (DANGER: relay secret then rides in every share
link — disposable tokens only). Stopping the run (`veld stop`) auto-unshares its
shares, and a consumer's join self-tears-down when the tunnel closes.

**Public web sharing** (`veld share --web`): exposes the `http` ports that have
`web` in their `share.expose` to anyone with a browser — no Veld on the viewer's side.
Requires `sharing.gateway` in config (a URL, or `{ "url", "token" }` where
`token` is a secret source like relay tokens; the org's self-hosted
`veld-gateway` container serves the public URLs — see docs/gateway.md). The
command prints deterministic `https://<slug>.<gateway-domain>` URLs and —
**by default — a viewer password**: the gateway shows a password page before
serving, a session cookie (12 h, capped at the share TTL) keeps the viewer in.
`--password <pw>` chooses the password (min 8 chars); the printed one-link
(`https://…/#veld-key=…`) carries it in the URL fragment (never hits DNS/logs).
Opt a service out with `"web": { "access": "link" }` in its `share` block (or
`--access link` for config-silent services; explicit config always wins over
the flag) — then the unguessable slug is the only gate, treat the link as a
secret. Multi-service caveat: the session cookie is per public host, so a
password-protected API called cross-origin from the shared frontend gets 401s
— give API nodes `"web": { "access": "link" }`. Web shares default to a 60-minute
TTL and peer shares to 120 (`sharing.webTtlMinutes` / `sharing.peerTtlMinutes`, or the
project's `sharing.web_ttl_minutes` / `peer_ttl_minutes`; `--ttl` overrides both). Web and peer are separate shares with separate
capabilities: `veld unshare` on one never affects the other. The toolbar arc
menu has a top-level **Sharing** item (dot when the page is web-shared) whose
submenu covers **Start/Stop sharing** (toggle a web share for the page's run
from the browser), **Copy public URL** (turn the current page into its public
deep link, path + query + hash preserved), and **Sharing status**. Transport
detail is not shown in the toolbar — `veld shares` prints each live tunnel's
transport (`relayed via <relay>` means throughput is capped by that relay, the
usual cause of slow shares; `direct` is full bandwidth), as does the management
UI.
Fidelity is best-effort:
the app sees its own origin `Host` (Vite allowedHosts pass), public host
rides in `X-Forwarded-Host`, redirects between shared services are rewritten,
WebSockets/HMR work; hard-coded absolute URLs / CORS / OAuth redirect URIs
are the operator's domain setup.

---

`veld skills` lists every topic. This document describes the veld binary that printed it — run `veld -V` if you need the version.

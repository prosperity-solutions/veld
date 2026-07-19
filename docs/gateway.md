# veld-gateway — operator guide

`veld-gateway` is the self-hosted server that makes `veld share --web` work:
a headless Veld peer that joins a developer's share over iroh and
reverse-proxies the tunneled service onto a real public URL
(`https://<slug>.<your-domain>`). One instance serves a whole org; many
environments can point at it.

It is stateless by design — registrations are heartbeat leases from the
developers' daemons, and public URLs are deterministic hashes, so a gateway
restart changes nothing: the next heartbeat re-establishes every share on the
same URLs. No database, no volume required.

## Quick start (Docker)

The gateway speaks plain HTTP; put TLS in front of it. The two options are a
TLS-terminating load balancer / ingress (below), or built-in TLS with a
mounted wildcard cert (`VELD_GATEWAY_TLS_CERT`/`_KEY`, see the reference). The
minted URLs are always `https://` — a gateway with no TLS in front will hand
out `https://` links to a plain-HTTP port, which fail to connect.

```sh
# Behind a TLS-terminating LB that routes *.share.acme.internal → this container.
docker run -d --name veld-gateway -p 8080:8080 \
  -e VELD_GATEWAY_DOMAIN=share.acme.internal \
  -e VELD_GATEWAY_TOKEN='<a long random secret>' \
  -e VELD_GATEWAY_RELAYS=https://relay.acme.internal \
  -e VELD_GATEWAY_RELAY_TOKEN='<relay token, if the relay is token-gated>' \
  ghcr.io/prosperity-solutions/veld-gateway:latest
```

Then, per environment (`veld.json`), opt a service into the `web` audience and
point at the gateway:

```jsonc
{
  "sharing": {
    "relays": ["https://relay.acme.internal"],
    // The token is resolved in the DAEMON's environment, not your shell — an
    // `export … && veld share` won't reach a background daemon. For a quick
    // start use a literal ("token": "…"); for real deployments prefer
    // { "file": "/run/secrets/gw-token" }. See "Injecting the token" below.
    "gateway": { "url": "https://share.acme.internal", "token": { "file": "/run/secrets/gw-token" } }
  },
  "nodes": {
    "frontend": {
      "variants": {
        "local": { "type": "start_server", "command": "npm run dev",
                   "share": { "expose": ["web"] } }
      }
    }
  }
}
```

```sh
veld share --web            # prints https://<slug>.share.acme.internal + a viewer password
```

Web shares are **password-protected by default** — see
[Viewer access control](#viewer-access-control-passwords). A service can opt
out with `"share": { "expose": ["web"], "web": { "access": "link" } }`.

**Injecting the token into the daemon.** `{ "env": … }` / `{ "file": … }` /
`{ "command": … }` are read by the veld **daemon** process (launchd/systemd/the
foreground `veld` you started), never your interactive shell. A literal token
in `veld.json` always works but lands in version control; a `file` under
`/run/secrets` is the usual production choice; for `env`, set the variable in
the daemon's service definition (not a shell `export`). `command` partially
excepts itself from the bare-daemon-environment rule: it runs with your
login-shell `PATH` (resolved the same way liveness probes do), so a
secret-manager CLI like `op` is *found* just as in your terminal — but only
`PATH` is inherited, not the rest of your shell environment. A command that
needs an exported variable (`OP_SERVICE_ACCOUNT_TOKEN`, `VAULT_ADDR`) or an
alias will still not see it; inject those via the daemon's service
definition. On a headless gateway container there is no user shell config —
resolution falls back to the container's `PATH`, so bake required CLIs into
the image and set `PATH` there.

## What you must provide

1. **A base domain** with a **wildcard DNS record**: `*.share.acme.internal`
   (and `share.acme.internal` itself) → the gateway. The apex serves the
   registration API; every share surfaces as a one-label subdomain.
2. **TLS for that wildcard.** Two supported modes:
   - **External termination** (recommended on PaaS/Kubernetes): your load
     balancer / ingress owns the wildcard cert and forwards plain HTTP to the
     gateway. Unlike the iroh relay (raw L4), the gateway is ordinary HTTP —
     L7 platforms work. The platform must route wildcard hosts.
   - **Built-in TLS**: mount a wildcard cert and set
     `VELD_GATEWAY_TLS_CERT` / `VELD_GATEWAY_TLS_KEY` (PEM paths).
3. **A registration token** (`VELD_GATEWAY_TOKEN`): a long random secret.
   Every environment that registers shares must present it. The gateway
   refuses to start without one.

## Configuration reference

Env-var-first; a config file is optional (`--config /path` or
`VELD_GATEWAY_CONFIG`), and env always wins over the file.

| Env var | Config key | Default | Meaning |
|---|---|---|---|
| `VELD_GATEWAY_DOMAIN` | `domain` | — (required) | Public base domain; URLs are `https://<slug>.<domain>` |
| `VELD_GATEWAY_TOKEN` | `auth.token` | — (required) | Registration auth token (literal). |
| `VELD_GATEWAY_TOKEN_FILE` | `auth.token` | — | Read the token from a file instead (Docker/K8s secret mounts — preferred over the env literal) |
| `VELD_GATEWAY_LISTEN` | `listen` | `0.0.0.0:8080` | Bind address |
| `VELD_GATEWAY_TLS_CERT` / `_KEY` | `tls.cert` / `tls.key` | unset | Wildcard cert/key (PEM). Unset = plain HTTP behind your TLS terminator |
| `VELD_GATEWAY_RELAYS` | `relays` | unset | `public`, or comma-separated relay URLs. When a list is set it is also an **allow-list**: tickets naming other relays are refused |
| `VELD_GATEWAY_RELAY_TOKEN` | *(per-entry `token` in file)* | unset | Auth token presented to the listed relay(s) |
| `VELD_GATEWAY_LEASE_SECS` | `lease_secs` | `90` | Registration lease; origin daemons heartbeat inside it |
| `VELD_GATEWAY_STATE_DIR` | `state_dir` | platform data dir | Where the persistent iroh node key lives (optional volume) |
| `VELD_GATEWAY_MAX_REGISTRATIONS` | `max_registrations` | `512` | Hard cap on concurrently live + in-flight shares; bounds a leaked token's blast radius. Raise for a large fleet — share #N+1 is refused with a clear error |
| `VELD_GATEWAY_TRUST_FORWARDED` | `trust_forwarded_headers` | `false` | Trust the immediate upstream LB's `X-Forwarded-For`: its last entry becomes the client IP (password rate-limit keying) and the chain is forwarded upstream. **Enable this when the gateway sits behind a TLS-terminating LB** — otherwise every viewer shares the LB's IP and a few wrong passwords rate-limit everyone. Leave off when the gateway is the direct internet edge (an inbound chain would be viewer-spoofable). Deliberately does not affect `X-Forwarded-Host` |
| `VELD_GATEWAY_TRUST_FORWARDED_HOST` | `trust_forwarded_host` | `false` | Trust `X-Forwarded-Host` (first entry) as the host the viewer addressed: it overrides `Host` for slug routing, the upstream `X-Forwarded-Host`, and Referer rewriting. Required behind a CDN that rewrites `Host` to its origin (see [Behind a CDN](#behind-a-cdn-cloudfront)). Enable ONLY behind an edge that **overwrites or strips** inbound `X-Forwarded-Host` — an edge that passes it through lets viewers inject the host your apps see |

File form (all fields optional, `SecretSource` accepted for secrets):

```jsonc
{
  "domain": "share.acme.internal",
  "listen": "0.0.0.0:8080",
  "tls": { "cert": "/certs/wild.pem", "key": "/certs/wild.key" },
  "auth": { "token": { "file": "/run/secrets/gw-token" } },
  "relays": [{ "url": "https://relay.acme.internal", "token": { "env": "RELAY_TOKEN" } }],
  "lease_secs": 90,
  "max_registrations": 512,
  "trust_forwarded_headers": false,
  "trust_forwarded_host": false
}
```

## How it behaves

- **Registration API** (apex only): `POST /api/v1/shares` with the share
  ticket, Bearer-authenticated. The same call is the heartbeat — idempotent,
  refreshes the lease. `DELETE /api/v1/shares/{id}` unregisters (also
  idempotent). Driven entirely by `veld share --web`; you never call it by
  hand.
- **Browser-facing pages are branded**: the apex `/` serves a small index
  page identifying the server as a Veld gateway; unknown slugs answer a
  branded "Share not found" 404 (unknown hosts and unmatched apex paths get a
  generic branded 404); password-protected slugs show a branded login; a dead
  tunnel or unresponsive upstream gets a branded 502/504. All
  pages follow [docs/branding.md](branding.md), are fully self-contained
  (inline CSS, no external assets), `noindex`, and deliberately static — no
  share metadata, counts, or hostnames are exposed to anonymous viewers.
- **Public URLs are deterministic**: `slug = hash(host machine ‖ hostname ‖
  share capability)` — 26 lowercase base32 chars, unguessable (the URL is the
  baseline access control), stable across gateway restarts, new per share.
- **Proxying**: one tunnel stream per HTTP request; WebSocket upgrades
  (dev-server HMR) are spliced through, with the `Origin` header dropped on
  the upgrade — dev servers (Next.js HMR) only accept `localhost`-ish origins
  on WebSockets and kill the socket otherwise, while an absent Origin passes.
  The origin service sees its own
  hostname in `Host` (dev-server allow-lists pass); the public host is in
  `X-Forwarded-Host`. `Location` redirects to shared sibling hostnames are
  rewritten to their public URLs; `Set-Cookie` `Domain` attributes naming
  origin hostnames are stripped (host-only cookies work publicly). Bodies are
  never rewritten.
- **Cleanup is layered**: the moment a developer unshares / stops the run /
  loses the daemon, the tunnel closes and the URLs die (the live connection is
  authoritative). The lease is a backstop that only reaps a registration whose
  tunnel is *already* closed — a missed heartbeat over a transient HTTPS blip
  never tears down a healthy share.

### Fidelity limits (what the operator owns)

`web` is best-effort by design — the gateway rewrites headers, never bodies.
Two consequences to know before sharing a non-trivial app:

- **Absolute URLs built from `Host` (SSR/OAuth).** The origin service sees its
  own hostname in `Host` (so dev-server host allow-lists pass zero-config), and
  the public host is in `X-Forwarded-Host` / `X-Forwarded-Proto: https`. An app
  that builds absolute URLs from `Host` — SSR canonical/asset links, OAuth
  `redirect_uri`, `build_absolute_uri` — will emit `*.localhost` URLs that a
  public viewer can't reach unless it honours `X-Forwarded-*`. Configure the
  framework to trust forwarded headers (most have a one-liner), and register
  OAuth redirect URIs against the public host. A relative-URL SPA needs none of
  this.
- **Cookies and CORS across services.** Each shared service gets its own
  unrelated slug host, so a session cookie scoped to a shared parent domain
  can't span them, and cross-service calls are cross-origin. The gateway
  rewrites `Set-Cookie Domain` (origin host **or a parent**) to host-only and
  rewrites `Access-Control-Allow-Origin` that echoes an origin host to the
  public origin — but an app that hard-codes a different origin in its CORS
  allow-list must add the public origin itself. A `Content-Security-Policy`
  that names origin hostnames is **not** rewritten (it's a body-adjacent
  allow-list); relax it or trust the public host if you ship a strict CSP in
  dev. And because each non-upgrade request's `Host`/`Origin` is rewritten to
  the *target* service's origin (upgrade requests drop `Origin` entirely, see
  above), a service that inspects the calling service's `Origin` for its own
  CORS/CSRF decisions sees same-origin — or, on WebSockets, no origin — rather
  than the caller — fine for the single-service flagship, a fidelity gap for
  tightly-coupled multi-service shares.
- **Tunnel transport is logged.** On registration the gateway logs
  `share tunnel established` with `transport=direct|relayed` and `via=<addr>`,
  and ~15 s later `share tunnel path settled` with the post-hole-punching
  state plus `rtt_ms`. A tunnel that stays `relayed` is capped by that relay's
  throughput (n0's public relays throttle) — the first thing to check when a
  share feels slow. The developer sees the same picture in `veld shares` and
  the overlay's Web sharing card.
- **Health**: `GET /livez` (liveness — the process is up) and `GET /readyz`
  (readiness — safe to route traffic) both answer `ok` on any Host
  (container/LB probes included), so Kubernetes `livenessProbe` /
  `readinessProbe` and Docker `HEALTHCHECK` work without knowing the domain.
  The gateway has no warm-up phase or external dependency, so today readiness
  equals liveness — the endpoints are split so probe configs have stable,
  distinct targets if that ever changes. Readiness also stays `ok` through
  the SIGTERM drain: traffic shedding during a rolling restart is handled by
  the listener refusing new connections and the orchestrator removing the
  endpoint, not by the probe flipping. On Kubernetes, endpoint removal
  propagates asynchronously, so pair the probe with a short `preStop` sleep
  (a few seconds) to keep the listener accepting until the pod has left the
  Service endpoints — otherwise a rollout can surface brief connection
  resets. `GET /healthz` remains as a legacy
  alias for liveness. Logs go to stdout (`RUST_LOG` controls verbosity).
- **Shutdown**: SIGTERM drains gracefully (10s budget) — rolling restarts are
  safe; in-flight requests finish and heartbeats re-register.

## Behind a CDN (CloudFront)

A CDN in front of the platform LB adds a wrinkle: CloudFront (and most CDNs
with a custom origin) **rewrites `Host` to the origin's hostname** when
forwarding — and the platform underneath (e.g. an ingress that routes by its
own hostname) usually *requires* that, so you can't just forward the viewer's
`Host`. The gateway then sees the platform hostname instead of
`<slug>.<domain>` and answers 404 for every share.

The fix is two-sided:

1. **Carry the viewer's host in `X-Forwarded-Host`.** On CloudFront, attach a
   CloudFront Function (viewer request) that copies the viewer `Host` in. The
   assignment **overwrites** anything a viewer sent — that's load-bearing, not
   cosmetic: the gateway takes the *first* `X-Forwarded-Host` entry, so an
   edge that appended (or passed a viewer's value through) would let viewers
   pick the host your apps see. Keep the header edge-owned:

   ```js
   function handler(event) {
     var request = event.request;
     request.headers['x-forwarded-host'] = { value: request.headers.host.value };
     return request;
   }
   ```

   Add the distribution's alternate domain names for `<domain>` and
   `*.<domain>` (with a matching wildcard cert in ACM), and point DNS at the
   distribution.

   If the apex `<domain>` rides the distribution too, the **registration API
   flows through CloudFront**: the behavior must allow **all HTTP methods**
   (registration is a Bearer-authenticated `POST`/`DELETE`; CloudFront
   defaults to GET/HEAD, which turns every registration into a silent 403 —
   viewer proxying works, but no share ever appears). CloudFront forwards the
   `Authorization` header automatically for non-GET/HEAD methods once they
   are allowed, so no extra header config is needed. Alternatively, point the
   apex's DNS straight at the platform LB and route only `*.<domain>` through
   CloudFront — registrations don't benefit from a CDN anyway.

2. **Set `VELD_GATEWAY_TRUST_FORWARDED_HOST=true`** so the gateway routes by
   that `X-Forwarded-Host`. It is a separate opt-in from
   `VELD_GATEWAY_TRUST_FORWARDED` on purpose: each trusts a different header
   with a different failure mode.

Rate-limiting caveat with a CDN in front: `VELD_GATEWAY_TRUST_FORWARDED`
keys the password rate limit on the **last** `X-Forwarded-For` entry — the
one your platform LB appended, which behind a CDN is the CDN's edge IP, not
the viewer's. Password attempts are then budgeted per CDN edge (all viewers
behind one PoP share a bucket), and the per-slug limit is the effective
brute-force bound. That's a coarser key than the LB-only setup, not a
security hole — but know it before you wonder why two colleagues behind the
same PoP can rate-limit each other. Per-viewer keying through a CDN needs a
trust-depth knob the gateway doesn't have yet.

Caching note: leave the distribution's default behavior at "no caching" (or
forward all headers/cookies/query strings) — shares proxy live dev servers and
password-gated content, which a shared URL-keyed cache must not store.

## Viewer access control (passwords)

Web shares are **password-protected by default** (SHARING_V2.md §6.1). The
developer's daemon sends the access policy — per-hostname mode plus the share
password — inside the registration call, and re-sends it with every
heartbeat. The gateway keeps it in memory only: a restart forgets it, the
next heartbeat restores it, statelessness intact.

How a viewer gets in:

1. First request to a password-mode slug → `401` with a self-contained login
   page (no share metadata leaked; `noindex`; `no-store`).
2. The form POSTs to `/__veld_gateway__/auth` on the slug host (a reserved
   path prefix — `/__veld_gateway__/` never reaches the origin service).
   A `#veld-key=…` URL fragment auto-fills and submits the form (the
   "one-link" flow); fragments never reach the gateway or its logs.
3. Correct password → a session cookie scoped to that slug host
   (`HttpOnly; Secure; SameSite=Lax`), then a redirect to the originally
   requested path. The cookie is **stripped before proxying** — the origin
   service never sees it.

Sessions are stateless signed tokens: the signing key is derived from the
share's capability, so the gateway needs no session store, restarts don't log
viewers out, and unsharing (which rotates the capability next time) kills all
sessions. Lifetime: 12 h, capped at the share's own expiry.

Brute force: password comparison is constant-time, and attempts are throttled
per client IP (10/min) **and** per slug (300/min across all IPs, so a
distributed guess is bounded without making viewer lockout trivial). The
limiter is in-memory; behind an external LB set
`VELD_GATEWAY_TRUST_FORWARDED=true` or all viewers share the LB's IP budget.

Two operational requirements for the password flow:

- **Viewers must reach the gateway over HTTPS.** The session cookie is
  `__Host-`-prefixed and `Secure`; a plain-HTTP viewer path (no TLS terminator
  in front of a plain-HTTP gateway) makes the browser drop it — the viewer
  enters the correct password and lands back on the login page, forever. The
  minted URLs are always `https://`, so this only bites broken deployments.
- **No URL-keyed shared cache in front of password slugs.** The gateway
  defaults password-mode responses to `Cache-Control: no-store` when the app
  doesn't set its own caching policy, but an app that says `public` is taken
  at its word — don't put a shared cache in front of protected content.

Nodes with `share.web.access: "link"` in the developer's config skip all of
this: anyone with the URL is served, the unguessable slug being the only
gate, and the reserved path prefix is not intercepted (fully transparent
proxying).

**Version skew**: the gateway acks the applied policy in the registration
response. A daemon that requested password protection from a gateway too old
to enforce it (no ack) tears the share down instead of publishing it open —
upgrade the gateway image to accept password-protected shares.

## Security model

- The registration API is never open: no token, no start. Token comparison is
  constant-time; the token is resolved once at boot (rotation = restart).
- The gateway only ever dials relays from **its own configuration** — a
  hostile registration cannot direct it to an attacker's relay (allow-list)
  or extract relay credentials (tokens never come from tickets).
- Share capability + host approval still gate the gateway's join like any
  peer; it appears as `gateway <domain>` in approval flows.
- The public surface for unknown hosts/slugs is a content-free 404.
- Viewer access is password-gated by default (above); the share password
  lives only in gateway memory and is never logged. Unauthenticated requests
  are answered before any tunnel stream is opened, so they cost the
  developer's machine nothing.
- Link-access slugs (`share.web.access: "link"`) rely on the URL alone: treat
  those links as secrets and keep TTLs short.

## Sizing & placement

CPU/memory needs are modest (it forwards bytes; TLS and QUIC are the cost).
Place it near your relay for latency. It must be reachable by: the browsers
of your viewers (HTTPS in), and your relay/hosts (iroh out). It does not need
to be reachable *by* the origin daemons directly — registration goes to the
apex over HTTPS, and tunnel traffic flows over iroh.

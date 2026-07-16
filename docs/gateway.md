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

```sh
docker run -d --name veld-gateway -p 8080:8080 \
  -e VELD_GATEWAY_DOMAIN=share.acme.internal \
  -e VELD_GATEWAY_TOKEN='<a long random secret>' \
  -e VELD_GATEWAY_RELAYS=https://relay.acme.internal \
  ghcr.io/prosperity-solutions/veld-gateway:latest
```

Then, per environment (`veld.json`):

```jsonc
{
  "sharing": {
    "relays": ["https://relay.acme.internal"],
    "gateway": { "url": "https://share.acme.internal", "token": { "env": "VELD_GW_TOKEN" } }
  }
}
```

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

File form (all fields optional, `SecretSource` accepted for secrets):

```jsonc
{
  "domain": "share.acme.internal",
  "listen": "0.0.0.0:8080",
  "tls": { "cert": "/certs/wild.pem", "key": "/certs/wild.key" },
  "auth": { "token": { "file": "/run/secrets/gw-token" } },
  "relays": [{ "url": "https://relay.acme.internal", "token": { "env": "RELAY_TOKEN" } }],
  "lease_secs": 90
}
```

## How it behaves

- **Registration API** (apex only): `POST /api/v1/shares` with the share
  ticket, Bearer-authenticated. The same call is the heartbeat — idempotent,
  refreshes the lease. `DELETE /api/v1/shares/{id}` unregisters (also
  idempotent). Driven entirely by `veld share --web`; you never call it by
  hand.
- **Public URLs are deterministic**: `slug = hash(host machine ‖ hostname ‖
  share capability)` — 26 lowercase base32 chars, unguessable (the URL is the
  baseline access control), stable across gateway restarts, new per share.
- **Proxying**: one tunnel stream per HTTP request; WebSocket upgrades
  (dev-server HMR) are spliced through. The origin service sees its own
  hostname in `Host` (dev-server allow-lists pass); the public host is in
  `X-Forwarded-Host`. `Location` redirects to shared sibling hostnames are
  rewritten to their public URLs; `Set-Cookie` `Domain` attributes naming
  origin hostnames are stripped (host-only cookies work publicly). Bodies are
  never rewritten.
- **Cleanup is layered**: the moment a developer unshares / stops the run /
  loses the daemon, the tunnel closes and the URLs die; a lost DELETE is
  covered by the lease expiring; an idle zombie is reaped by the sweeper.
- **Health**: `GET /healthz` answers `ok` on any Host (container/LB probes
  included). Logs go to stdout (`RUST_LOG` controls verbosity).
- **Shutdown**: SIGTERM drains gracefully (10s budget) — rolling restarts are
  safe; in-flight requests finish and heartbeats re-register.

## Security model

- The registration API is never open: no token, no start. Token comparison is
  constant-time; the token is resolved once at boot (rotation = restart).
- The gateway only ever dials relays from **its own configuration** — a
  hostile registration cannot direct it to an attacker's relay (allow-list)
  or extract relay credentials (tokens never come from tickets).
- Share capability + host approval still gate the gateway's join like any
  peer; it appears as `gateway <domain>` in approval flows.
- The public surface for unknown hosts/slugs is a content-free 404.
- **What the gateway does NOT provide (yet)**: per-viewer authentication.
  Anyone with a public URL can use it while the share lives. Treat links as
  secrets and keep TTLs short. Viewer-facing access controls (passwords,
  approval) are a planned increment — see SHARING_V2.md §6.

## Sizing & placement

CPU/memory needs are modest (it forwards bytes; TLS and QUIC are the cost).
Place it near your relay for latency. It must be reachable by: the browsers
of your viewers (HTTPS in), and your relay/hosts (iroh out). It does not need
to be reachable *by* the origin daemons directly — registration goes to the
apex over HTTPS, and tunnel traffic flows over iroh.

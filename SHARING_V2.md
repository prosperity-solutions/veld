# Sharing v2 — Declarative policy + public web sharing

Status: **Draft RFC** · Owner: Peter · Branch: `sharing-level-two`

This RFC extends Veld's peer-to-peer sharing in two directions:

1. **Declarative, compliant sharing** — relay policy and per-service opt-in move
   into `veld.json`, so an environment can only ever share what it explicitly
   allows, over relays the org controls.
2. **Public web sharing** — a way to expose a shared service to someone who does
   **not** run Veld (a designer, a coworker, a stakeholder) via a real public
   URL.

It deliberately keeps these as separate, independently shippable increments.

---

## 1. Where we are today

Sharing is entirely **runtime and imperative**. There is no config surface.

| Concern | Today | File |
|---|---|---|
| Relay | n0's **public** relays via `presets::N0`; single override via env `VELD_SHARE_RELAY` | `crates/veld-daemon/src/share/endpoint.rs:63,67-73` |
| Service selection | `veld share` mints a manifest of **every** URL-bearing node in the run; only runtime `--node` filters it | `crates/veld-daemon/src/share/api.rs:213-237` |
| Opt-in | **None.** No config flag marks a service shareable | `crates/veld-core/src/config.rs` (no share fields) |
| URL fidelity | Host ships the **literal** hostname; consumer reproduces the exact URL via Caddy + DNS | `crates/veld-core/src/share.rs:29-45`, `crates/veld-daemon/src/share/manager.rs:405-418` |
| Audience | **veld ↔ veld only** | — |
| Access control | Capability bearer token + `ApprovalMode {First, Manual, Auto}` | `crates/veld-core/src/share.rs:16-24,70-95` |

**The load-bearing property:** peer sharing reproduces the origin's hostname
*verbatim* on the consumer. Redirects, cookies, CORS, and OAuth redirect URIs
all keep working — precisely because both peers run identical Veld setup
(same scheme, same port, same fake TLD). Every design decision below is measured
against whether it preserves or breaks this.

## 2. Goals / non-goals

**Goals**
- Relay policy is declared in config: `public`, or an explicit list of
  self-hosted iroh relay URLs. Multi-relay.
- A service is shareable **only** if config marks it so. `veld share` refuses
  anything not opted in. This is the compliance story.
- A shared service can optionally be reachable from a plain browser with no Veld
  installed, via a real public URL.
- A Veld user can trivially copy the public URL for what they're looking at
  (host swapped, path + query preserved) from the overlay toolbar.

**Non-goals (this RFC)**
- Replacing the capability/approval security model — it stays.
- Guaranteeing full URL fidelity for public web sharing — `web` rewrites the
  host; matching the domain to the app is the **operator's** responsibility (§5).
- Shipping a browser-native iroh viewer (§7 — spike only).

## 3. Config design

Two axes, deliberately **not** conflated: *relay policy* (environment-wide) and
*shareability* (per service).

### 3.1 Relay policy — central block

```jsonc
{
  "sharing": {
    // Required to share — no implicit default. "public" or a list of relay URLs.
    "relays": "public",
    // "relays": ["https://relay.acme.internal", "https://relay2.acme.internal"],

    // Public web gateway this environment points at (only needed for expose: web).
    "gateway": "https://share.acme.internal"
  }
}
```

- **Explicit opt-in required.** There is no implicit default: `veld share` is
  refused unless a relay is chosen (config, or the `VELD_SHARE_RELAY` env). This
  is deliberate — nothing is ever routed over n0's public relays by accident;
  even public must be named (`"relays": "public"`).
- `"public"` → `presets::N0`. A list → `RelayMode::custom([...])` with **all**
  listed relays.
- `VELD_SHARE_RELAY` remains an override for ad-hoc/testing; config wins when
  both are present.
- Asymmetry: the explicit-opt-in requirement applies to **hosting** (`veld
  share`). The **join** path has no project config and defaults to public (or
  the env relay) so a bare `veld join <ticket>` keeps working — joining consumes,
  it doesn't expose.

Self-hosting an iroh relay is a single Docker container, so the compliance ask
("no traffic through n0's public relays") is cheap to satisfy.

### 3.2 Shareability — per-variant `share` block

Co-located on the variant, matching where `probes` / `recovery` already live.
**Absence means the service can never be shared.**

```jsonc
{
  "nodes": {
    "frontend": {
      "variants": {
        "local": {
          "share": { "expose": ["peer"] }        // veld ↔ veld, verbatim URL (strong)
        }
      }
    },
    "api": {
      "variants": {
        "local": {
          "share": { "expose": ["peer", "web"] }  // both audiences at once
        }
      }
    }
  }
}
```

`expose` values:

| Value | Audience | URL fidelity | Mechanism |
|---|---|---|---|
| `peer` | Other Veld users | **Verbatim** — exact origin URL reproduced | Today's iroh peer + Caddy/DNS reproduction |
| `web` | Anyone with a browser | **Best-effort** — rewritten host (see §5) | Public gateway server |

Naming rationale: `peer` vs `web` names the *mechanism and reach*, and — unlike
`private`/`public` — signals that `web` is a weaker guarantee, so users don't
expect peer-grade fidelity and get burned.

`expose` is a **list** so one service can serve both a Veld coworker (`peer`)
and a non-Veld stakeholder (`web`) at once, without re-config to switch
audiences. Empty list / absent `share` = not shareable.

### 3.3 Enforcement

`build_manifest` (`api.rs:213-237`) changes from "every URL-bearing node" to
"every node whose active variant has a `share` block permitting the requested
mode." `veld share` on a service with no `share` block is a hard error with a
message pointing at the config field. The runtime `--node` filter still narrows
*within* the opted-in set; it can never widen it.

## 4. Relay wiring change

`bind_endpoint` (`endpoint.rs:62-76`) takes relay policy from resolved config
instead of reading a single env var:

- `"public"` → `presets::N0` (unchanged).
- list → `RelayMode::custom([url, url, ...])`, all parsed; invalid entries warn
  and are dropped; if *all* are invalid, fail loudly rather than silently
  falling back to public (a silent fallback to n0 would be a compliance
  violation — the whole point was to avoid public relays).

**Relay auth tokens (shipped).** A self-hosted relay entry may be an object
`{ "url", "token" }` instead of a bare URL string. `token` is a `SecretSource`:
a literal string, or `{ "env" | "file" | "command" }` so the secret can be read
from the daemon's environment, a file (Docker/K8s secret mounts), or a
secret-manager CLI (`op read …`) rather than living in `veld.json`. iroh sends it
as an `Authorization: Bearer <token>` header (`RelayConfig::with_auth_token`), a
lightweight "you need the shared secret to use our relay" gate — not per-user
identity. The token *declaration* is part of the endpoint-map key (so it stays a
cheap, hashable key with no resolved secret in it); resolution happens at
`bind_endpoint` time and a token that fails to resolve is a hard error (never
bind unauthenticated). `VELD_SHARE_RELAY_TOKEN` pairs a literal token with the
`VELD_SHARE_RELAY` env override.

**One endpoint per relay policy.** The daemon binds a distinct iroh endpoint for
each relay policy on demand, held in a `Map<RelayChoice, Endpoint>`. Each share /
join routes to the endpoint matching its policy, so shares on different relays
(public + a self-hosted relay) run **concurrently** — no single-policy limitation,
no conflict, no restart.
- **Node identity:** iroh requires one identity per endpoint. The public endpoint
  reuses the daemon's persistent key (unchanged behavior + stable identity);
  each custom-relay endpoint gets a fresh `SecretKey::generate()` per run. Shares
  are ephemeral (die with the daemon), so a custom endpoint's node id need not
  survive a restart.
- **Bookkeeping:** each endpoint runs its own accept loop; the reaper is global
  (scans all shares) and starts once. Capability matching is per-share and
  endpoint-agnostic, so auth is unchanged.
- **Bind race:** `get_or_bind` holds the map lock across the (infrequent) bind so
  two callers racing on the same policy can't double-bind.

Remaining behavior notes (documented in the error paths + README):
- The custom-relay guarantee covers the host's outbound leg; the consumer's join
  binds env-or-public independently and must configure matching relays for
  end-to-end confinement.
- `sharing.relays` is resolved from **live** config at share time, not a snapshot
  from `veld start` — editing it mid-run changes the next share.
- `VELD_SHARE_RELAY` is read from the daemon's environment (not the caller's
  shell) and config `"public"` overrides it (not an enforceable floor).

## 5. Public web sharing — one architecture, honest limits

The two shapes floated (a "mirror the fake TLD" proxy vs a "mint a public URL"
gateway) are the **same server** with different URL strategies:

> A headless Veld peer that `join`s the share over iroh, then reverse-proxies the
> tunneled service onto a public HTTP endpoint.

The only real question is the public URL, and here is the hard constraint:

**The verbatim-hostname property cannot survive the jump to the public web.**

- **Mirror the fake TLD** — `*.localhost` / fake apex domains cannot resolve on
  public DNS. Dead end for arbitrary browsers.
- **Mint a real public URL** — the gateway owns a real wildcard domain and mints
  `https://<slug>.share.<gateway-domain>` per shared service. Viable — but it
  **reintroduces the exact host-rewrite problem peer sharing avoids**:
  - absolute URLs baked into the app point at the origin host,
  - cookies are scoped to the original host,
  - CORS allow-lists and OAuth redirect URIs reference the original host.

So **web sharing is a weaker guarantee than peer sharing** — but this is an
**operator responsibility, not a Veld defect.** Veld provides the tunnel + the
proxy; the operator is responsible for standing up the gateway on a domain that
works for their app (right base domain, TLS, CORS, cookie scope, OAuth redirect
URIs registered against the public host). Apps with relative URLs work out of the
box; apps with absolute-host assumptions require the operator to configure the
domain to match. Veld's job is to make host rewriting predictable and to document
what the operator must own — not to magically preserve verbatim fidelity (that's
the `peer` mode's job). This is *why* the config uses a different word (`web`).

### 5.1 One binary or two?

**Decision: a fourth workspace binary, `veld-gateway` (new crate
`crates/veld-gateway`).** Alternatives considered and rejected:

| Option | Why not |
|---|---|
| Subcommand of the `veld` CLI (`veld gateway serve`) | The CLI is deliberately thin — it has **no iroh, no HTTP-server deps** today (`crates/veld/Cargo.toml`); it talks to the daemon over IPC. Embedding the gateway drags iroh + hyper + TLS into every developer install for code that only ever runs on a server. |
| Mode of `veld-daemon` (`veld-daemon --gateway`) | The daemon assumes its habitat: a privileged `veld-helper` peer (DNS/Caddy IPC), local run state, feedback, GC, macOS conventions. None of that exists on a Linux host. Running it public-facing means shipping all that dead machinery as attack surface, and every daemon refactor risks the server path. |
| Separate repo | Guarantees protocol drift. The whole point of §5.2 is that host and gateway compile against the same transport crate in one workspace, versioned in lockstep. |

The marginal cost is low: the release pipeline already builds three binaries
for four targets (macOS + Linux, x86_64 + aarch64), so a fourth binary is
mechanical. Operationally it mirrors the self-hosted iroh relay: **one more
container in the org's stack**, and the compliance boundary stays auditable —
"the thing that exposes services publicly" is a distinct, small artifact, not a
flag on the dev tool.

### 5.2 Shared transport crate — killing duplication and drift

The gateway is "a headless peer that joins" — so it must speak *exactly* the
protocol the daemon speaks, forever. We get that structurally, not by
discipline: extract the transport layer out of `veld-daemon` into a new
library crate **`crates/veld-share`**.

**Layering after the extraction:**

| Crate | Contains | iroh dep |
|---|---|---|
| `veld-core` (unchanged role) | Pure wire/config types: `Capability`, `ShareManifest`, `ShareTicket`, `SharingConfig`, `RelayPolicy`, `SecretSource`, DTOs | no |
| `veld-share` (**new**) | `ALPN`, `proto` (control/open-stream frames), `forward::splice`, `join::{dial, forward_local}`, `host::{HostShare, read_control, accept_and_serve, deny}`, `RelayChoice` + `bind_endpoint` + relay-token resolution + `relay_auth_status`, secret-key persistence | yes |
| `veld-daemon` | What is genuinely daemon-shaped: `ShareManager` lifecycle (Caddy/helper routes, approval UX, reaper, join watcher), HTTP API, interactive relay-token cache (`token_store`) | via `veld-share` |
| `veld-gateway` (**new**) | Registration API, slug router, HTTP front + rewrites (§5.3) | via `veld-share` |

Both *halves* of the protocol (host and join) live in `veld-share` even though
the gateway only joins — keeping them in one crate means the loopback tunnel
test (today in `share/mod.rs`) moves there and exercises both sides against
each other on every build.

**Drift guards, concretely:**
- One `ALPN` constant (`veld/share/1`) in one crate; a protocol change is a
  version bump in exactly one place, and both binaries pick it up or neither.
- Wire types stay serde structs in `veld-core` with the existing
  compat conventions (`#[serde(default)]`, skip-if-empty).
- A cross-crate integration test in `veld-gateway`: spin up an in-process
  `HostShare` (daemon's host half), register it with a gateway instance, drive
  an HTTP request through the public front, assert the bytes round-trip. The
  gateway can't drift from the daemon without this failing.
- Workspace lockstep versioning: all four binaries release together; a
  host/gateway version skew across orgs is handled by the versioned ALPN
  (connect fails loudly, never mis-speaks).

### 5.3 How the gateway binary works

Anatomy — four components in one process:

```
                 ┌───────────────────────── veld-gateway ─────────────────────────┐
 origin daemon ──► 1. Registration API      2. Join engine        3. Slug router  │
 (HTTPS + token)  │   POST /api/v1/shares ──► veld-share::dial ──► slug → (conn,  │
                  │   DELETE /api/v1/…         over iroh             hostname)    │
                  │                                                      ▲        │
 browser ─────────► 4. HTTP front: TLS for *.share.<domain> ─────────────┘        │
                  │    Host: <slug>.share.<domain> → OpenStream{hostname}         │
                  └────────────────────────────────────────────────────────────────┘
```

1. **Registration API.** The origin daemon (not the gateway) initiates: it
   `POST`s the share ticket plus a gateway auth token. The gateway validates
   the token, decodes the ticket, and joins over iroh **as an ordinary peer**
   — same relay-confinement rules (it dials the relays the ticket advertises,
   never falls back to public), same capability gate, same approval flow on
   the host (it shows up labeled `gateway <domain>`, so `manual` mode lets the
   user see and approve the gateway like any joiner).
2. **Join engine.** `veld-share::join::dial` verbatim. Holds the live
   `Connection`; when it closes (host unshared/stopped/crashed), all slugs for
   that share are dropped — the exact mirror of the daemon's join watcher.
3. **Slug router.** Per manifest node, mint `https://<slug>.share.<domain>`.
   Slugs are **deterministic and unguessable**:

   ```
   slug = base32( SHA-256("veld-gateway-slug/1" ‖ host_node_id ‖ hostname ‖ capability)[..16] )
   ```

   - *Stateless*: the gateway recomputes the same slug from the registration
     alone — a gateway restart followed by the origin's heartbeat re-register
     yields the **same public URL**; no database needed for URL stability.
   - *Machine-bound*: `host_node_id` (the iroh node id from the ticket) ties
     the slug to the sharing machine; the same service shared from a different
     machine gets a different URL.
   - *Unguessable*: the capability (32-byte secret) is a hash input, so the
     slug inherits its entropy; the hash is one-way, so the slug leaks nothing.
     128 bits → 26 base32 chars, comfortably inside the 63-char DNS label
     limit. The URL itself is the baseline bearer secret (§6).
   - *Ephemeral by construction*: a new share (new capability — e.g. after a
     daemon restart, since shares die with the daemon) mints a new slug. Slug
     lifetime = share TTL.
4. **HTTP front.** Terminates TLS, resolves the slug from `Host`, opens a
   fresh bi-stream (`OpenStream{hostname}`), and speaks HTTP/1.1 over it to
   the origin's **plain-HTTP upstream port** — the same bytes the peer path
   produces (`host.rs` splices to `127.0.0.1:<port>` either way; in peer mode
   it's the consumer's Caddy that adds TLS, here it's the gateway). This front
   is an HTTP-aware proxy (hyper), *not* a raw TCP splice, because it must:
   - support `Upgrade`/WebSocket (splice raw after the 101 — HMR works),
   - set `X-Forwarded-For/Proto/Host`,
   - rewrite `Location`/`Refresh` response headers that name the origin's
     fake-TLD hostnames back to the public host.

   **Host header policy (revised while implementing):** the upstream `Host`
   is rewritten to the **origin hostname**. Dev servers enforce host
   allow-lists (Vite's `allowedHosts` default admits `*.localhost` but would
   reject an unknown public host), so origin-Host makes the flagship case —
   sharing a dev frontend — work zero-config; the public host travels in
   `X-Forwarded-Host`, and `Set-Cookie` `Domain` attributes scoped to origin
   hostnames are stripped (host-only cookies work on the public host).

   **What it does not do:** rewrite HTML/JS bodies. Absolute URLs baked into
   the app, CORS allow-lists, OAuth redirect URIs — operator responsibility,
   per the top of §5.

**Statelessness.** The gateway persists nothing. Registrations are leases: the
origin daemon heartbeats (re-`POST`s, idempotent) every N seconds; a gateway
restart loses all state and the next heartbeat re-establishes each share.
Unshare/expiry → the daemon `DELETE`s (best-effort; the lease expiring covers
the crash case). No database, no volume — restart-safe by construction.

### 5.4 Host-side flow and audience separation

`expose: ["web"]` in config is *permission*; distribution to the gateway is a
runtime act:

```
veld share --web          # requires ≥1 node with expose containing "web"
```

**Web sharing mints a separate share** (own capability, manifest = the
web-opted nodes only), whose ticket goes to the gateway — it is never pasted
to a human. The peer share (if any) stays its own share with its own
capability. Rationale: the alternative — one share whose manifest is filtered
per joiner type — needs the host to *trust* the joiner's self-declared type,
i.e. a protocol change and a new trust decision. Two shares keep the protocol
untouched and give capability isolation for free: revoking the web audience
(`veld unshare --web`) kills the gateway's capability without touching peers.

The registration response maps `origin hostname → public URL`; the daemon
stores this map for the share's lifetime. That map is what powers §5.6 and the
`veld share` output (the user immediately sees the public URLs).

### 5.5 Deployment & configuration (operator story)

One container, no privileges, mirrors the self-hosted relay's story. The
gateway is **env-var-first**: every setting has a `VELD_GATEWAY_*` variable,
and a config file is optional (for operators who prefer mounted config).
Env wins over file. A containerized deployment needs zero files:

| Env var | Config key | Meaning |
|---|---|---|
| `VELD_GATEWAY_DOMAIN` | `domain` | Public base domain; URLs are `https://<slug>.<domain>` |
| `VELD_GATEWAY_LISTEN` | `listen` | Bind address (default `0.0.0.0:8080`) |
| `VELD_GATEWAY_TLS_CERT` / `_KEY` | `tls.cert` / `tls.key` | Wildcard cert paths; unset = plain HTTP behind an external TLS terminator |
| `VELD_GATEWAY_TOKEN` | `auth.token` (SecretSource) | Registration auth token origins must present |
| `VELD_GATEWAY_RELAYS` | `relays` | Comma-separated relay URLs, or `public` |
| `VELD_GATEWAY_RELAY_TOKEN` | — | Auth token for the (single) custom relay, env form |

```jsonc
// gateway.json — optional file form (SecretSource reused for secrets)
{
  "domain": "share.acme.internal",     // public URLs: https://<slug>.share.acme.internal
  "listen": "0.0.0.0:8080",
  "tls": { "cert": "/certs/wild.pem", "key": "/certs/wild.key" },  // omit → external TLS
  "auth": { "token": { "env": "VELD_GATEWAY_TOKEN" } },  // what origins must present
  "relays": ["https://relay.acme.internal"]              // same contract as veld.json
}
```

**Container image.** The release pipeline builds and publishes a multi-arch
(amd64 + arm64) image to `ghcr.io/prosperity-solutions/veld-gateway` on every
release (i.e. every merge to `main` that produces one), tagged `<semver>` and
`latest`. The image reuses the already-built Linux release binaries (no second
compile), runs as non-root on a minimal base, and is configured entirely via
the env vars above.

```sh
docker run -p 8080:8080 \
  -e VELD_GATEWAY_DOMAIN=share.acme.internal \
  -e VELD_GATEWAY_TOKEN=…  \
  -e VELD_GATEWAY_RELAYS=https://relay.acme.internal \
  ghcr.io/prosperity-solutions/veld-gateway:latest
```

- **DNS:** one wildcard record, `*.share.acme.internal → gateway`.
- **TLS, two modes at v1:** (a) `external` — the platform's L7 load
  balancer/ingress terminates TLS and the gateway speaks plain HTTP behind it;
  (b) bring-your-own wildcard cert files (mounted secret). Built-in ACME
  DNS-01 is deferred — wildcard issuance needs DNS-provider credentials and
  drags a lego-sized dependency in; operators who want it run a cert-manager
  sidecar. Note the contrast with the iroh relay: the relay needs raw
  L4/TCP and breaks behind L7 gateways, but the **gateway is ordinary HTTP(S)
  and is explicitly L7-platform-friendly** — the only platform requirements
  are wildcard host routing and a wildcard cert.
- **Origin side:** `sharing.gateway` grows the same shape as relay entries —
  a bare URL string, or `{ "url": …, "token": <SecretSource> }` for the
  gateway auth token. String form stays valid (shorthand, like relays).
- **Health:** `/healthz` for the platform's checks; structured logs to stdout.

### 5.6 Copy-URL UX

The origin Veld user is looking at the app on its fake-TLD URL and cannot paste
that to a non-Veld user. So:

- The gateway exposes a **stable public URL** per shared service.
- The overlay toolbar (no longer "just feedback") gets a **Copy public URL**
  action that translates the *current browser location* → public URL by swapping
  the host and **keeping path + query** — so a deep link to a specific screen
  survives the copy.

## 6. Security & compliance

- Peer sharing security model (capability + approval) is unchanged for `peer`.
- `web` widens the audience to the open internet, so peer sharing's two gates
  (capability token + host approval of a *Veld peer*) are **not sufficient** —
  the viewer is an anonymous browser, not an authenticated peer. Web sharing
  needs its own access layer, evaluated in increment 2. Candidate controls,
  roughly in order of strength/effort:
  - **Per-share access token in the URL** — the minted public URL carries an
    unguessable token; no token, no access. Cheapest; leaks if the URL leaks.
  - **Share password** — gateway prompts for a password the host sets at share
    time (sent out-of-band). Simple mental model for non-technical viewers.
  - **Join approval for web viewers** — the gateway holds each new browser
    session until the host approves it (mirrors peer `manual` mode, surfaced in
    the overlay toolbar). Strongest; highest UX cost.
  - **Defaults** — `web` should default to a stricter posture than `peer`:
    non-`Auto` approval, a shorter TTL, and a token/password required (never an
    open URL by default).
  The exact combination is an increment-2 decision; the config must reserve room
  for it (e.g. a `share.web` sub-object for password/approval settings) so
  today's `expose: ["web"]` stays forward-compatible.
- Gateway-side allow-listing of which shares it will accept, plus explicit
  `expose: web` in config (never implicit), remain prerequisites.
- Compliance win is structural: with per-service opt-in + self-hosted relays,
  an environment provably cannot leak a service that wasn't declared, over a
  relay the org doesn't run.

## 7. Browser ↔ browser over iroh (deferred spike)

The idea: a non-Veld user's browser speaks iroh directly (WASM), no gateway.

Reality check: a browser cannot hold arbitrary QUIC/UDP; iroh in-browser needs
WebTransport/WebRTC, and the relay-over-WebSocket path is not a general peer
transport. Even if the viewer's browser ran an iroh JS client plus a service
worker acting as a local proxy, it **still hits the same-origin / host-rewrite
wall** — the browser enforces it. So this does not dodge the fundamental problem
of §5; it relocates the proxy into the viewer's browser.

Verdict: **research spike only**, after §5 ships. Not on the critical path.

## 8. Sequencing

Three independent increments; ship top-down.

1. **Config surface + enforcement** (compliance win, no new server) — *this PR*
   - `sharing.relays` (public | list) wired into `bind_endpoint`.
   - one endpoint per relay policy → concurrent different-relay shares.
   - per-variant `share.expose` opt-in; `veld share` enforces it.
   - Docs checklist (README, `docs/configuration.md`, skills, schema).
2. **Public web gateway** (new binary) — design in §5.1–5.6; ship in slices:
   - a. `veld-share` extraction (pure refactor, no behavior change) — its own PR.
   - b. `veld-gateway` binary: registration API + join engine + slug router +
     HTTP front (incl. WebSocket upgrade); container image + release wiring.
   - c. Host side: `veld share --web` (separate web share), `sharing.gateway`
     token form, heartbeat lease, public-URL output.
   - d. Overlay toolbar "Copy public URL".
   - e. Honest fidelity docs + operator guide (DNS/TLS/platform).
3. **Browser-native viewer** — spike only.

## 9. Decisions & remaining questions

**Decided**
- `expose` is a **list** (`["peer", "web"]`) — both audiences at once, §3.2.
- Gateway discovery: origin points at its gateway via `sharing.gateway` URL in
  config, §3.1. One gateway per environment (an org can point many envs at one
  shared gateway instance).
- Config relay policy **wins** over `VELD_SHARE_RELAY`; env stays as an ad-hoc
  override only when config is silent, §3.1/§4.
- Gateway is a **fourth workspace binary** (`veld-gateway`), not a CLI
  subcommand or daemon mode, §5.1.
- Duplication/drift solved structurally: transport extracted to a shared
  `veld-share` crate; one ALPN constant; cross-crate integration test, §5.2.
- Gateway ↔ share trust: a **gateway auth token** (SecretSource) the origin
  presents on registration; the gateway then joins as an ordinary peer through
  the existing capability + approval gates, §5.3.
- Web audience gets a **separate share** (own capability, web-opted nodes
  only) whose ticket goes only to the gateway — independent revocation, no
  protocol change, §5.4.
- Slugs: **deterministic** hash (host node id ‖ hostname ‖ capability →
  128-bit base32) — stateless URL stability across gateway restarts,
  machine-bound, unguessable. Lifetime = share TTL. Gateway holds no
  persistent state; registrations are heartbeat leases, §5.3.
- Gateway is **env-var-first** for containerized hosting; config file
  optional. Release pipeline publishes a multi-arch image to
  `ghcr.io/prosperity-solutions/veld-gateway` on each release, §5.5.

**Still open (defer to the relevant slice)**
1. Should `web` enforce stricter defaults (approval mode, shorter TTL) than
   `peer`? Password / per-viewer approval layer — config room reserved via
   `share.web` sub-object, §6.
2. Heartbeat/lease interval and jitter; `DELETE` vs pure lease-expiry
   semantics on unshare.
3. `host_header: "public" | "origin"` per-registration knob — ship in v1 or
   wait for a real vhost-routing case?
4. Human-readable slug aliases (e.g. `frontend-demo.share.…`) on top of the
   unguessable default — worth the collision/enumeration surface?

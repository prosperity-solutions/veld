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

### 5.1 Gateway server (new binary)

- Config: base public domain, TLS (ACME wildcard), which shares to accept
  (capability/allow-list), relay policy (shares the same `sharing.relays`
  contract).
- On accepting a share: `join` over iroh → mint slug → publish
  `https://<slug>.share.<domain>` → reverse-proxy with host rewriting → return
  the public URL to the origin.
- Runs anywhere reachable: one per org, self-hosted, same Docker-friendly story
  as the relay.
- Reuses the browser join-URL scaffolding already present
  (`manager.rs:737-746`).

### 5.2 Copy-URL UX

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
2. **Public web gateway** (new binary)
   - headless peer + reverse proxy + slug minting + host rewriting.
   - overlay toolbar "Copy public URL".
   - honest fidelity docs.
3. **Browser-native viewer** — spike only.

## 9. Decisions & remaining questions

**Decided**
- `expose` is a **list** (`["peer", "web"]`) — both audiences at once, §3.2.
- Gateway discovery: origin points at its gateway via `sharing.gateway` URL in
  config, §3.1. One gateway per environment (an org can point many envs at one
  shared gateway instance).
- Config relay policy **wins** over `VELD_SHARE_RELAY`; env stays as an ad-hoc
  override only when config is silent, §3.1/§4.

**Still open (defer to the relevant increment)**
1. Should `web` enforce stricter defaults (approval mode, shorter TTL) than
   `peer`? — settle in increment 2.
2. Wildcard slug scheme + collision/expiry semantics for public URLs. —
   increment 2.
3. Gateway ↔ share trust: how the gateway is allow-listed to accept a share
   (reuse capability? separate gateway token?). — increment 2.

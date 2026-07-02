# RFC: Peer-to-peer environment sharing (`veld share` / `veld join`)

**Status:** Draft / exploration
**Branch:** `p2p-based-sharing-exploration`

## Summary

Let a Veld user share a running environment with a colleague so the colleague
opens **the same URLs** (e.g. `https://frontend.my-feature.proj.localhost`) on
their own machine — with no cloud deploy, no Tailscale, no port numbers, and no
account. Both people already have Veld installed; that is the only requirement.

Transport is [iroh](https://github.com/n0-computer/iroh) (QUIC + NAT hole
punching + relay fallback). Naming, TLS, and routing reuse machinery Veld
**already has**: the privileged helper's DNS + Caddy route injection, and each
machine's own trusted internal CA.

## Motivation

Users want to show work in progress to teammates and clients. Today the only
answer is "deploy it" or "install a tunnel/VPN." Veld already owns a local DNS +
reverse proxy + CA per machine — so if two machines can exchange encrypted bytes,
one can *materialize the other's URLs locally* and route them over the wire. The
missing 20% is the transport and a manifest exchange; iroh supplies the transport.

## Non-goals

- **Zero-install browser consumers.** Explicitly out of scope. A consumer with
  nothing installed can't get a trusted cert for a `.localhost`/custom hostname,
  and `.localhost` can't route off-box. Sharing to a plain browser would require
  a public TLS-terminating relay gateway (ngrok-style) — a different product with
  different security/ops. Not this RFC.
- **A Veld-run coordination service.** v1 leans on iroh tickets + n0's free public
  relays. No accounts, no database, no Veld-hosted infra to launch.

## Why this fits Veld (what's already there)

| Need | Existing mechanism | Location |
|------|-------------------|----------|
| Inject a route for a hostname | Helper IPC `AddRoute { id, hostname, upstream }`, upstream is a `localhost:PORT` dial string | `crates/veld-core/src/helper.rs:68`, `crates/veld-helper/src/caddy.rs:178` |
| Make a hostname resolve | Helper IPC `AddHost`; `.localhost` needs no writes (RFC 6761), custom apex → dnsmasq + `/etc/hosts` | `crates/veld-helper/src/dns.rs:40` |
| Trusted TLS on the consumer side | Consumer's Caddy issues an internal cert from the consumer's own CA, already trusted from setup | `crates/veld-helper/src/caddy.rs` |
| Shareable manifest of a run | `RunState` → `NodeState` already stores literal `url` + `port` per node | `crates/veld-core/src/state.rs:176`, `:89` |
| A long-lived process to host a persistent QUIC socket | `veld-daemon` (always-on, `#[tokio::main]`, axum HTTP on `127.0.0.1:19899`) | `crates/veld-daemon/src/main.rs:66` |
| A local control plane for the CLI/dashboard | axum management router | `crates/veld-daemon/src/management.rs:22` |

**Key consequence:** the consumer side of sharing is nothing more than
`AddHost(hostname, 127.0.0.1)` + `AddRoute(hostname → localhost:<tunnel-port>)`.
No new privileged-helper surface is required. We also ship the **literal**
hostname strings from the host's manifest rather than recomputing them from the
URL template, so URLs match byte-for-byte on both machines and the whole
template-divergence problem disappears.

## Architecture

```
HOST (Alice)                                     CONSUMER (Bob)
------------                                     --------------
app @ localhost:4001                             browser
   ^                                                 |  opens https://frontend.my-feature.proj.localhost
   | localhost dial                                  v
veld-daemon                                       Bob's Caddy  (internal cert from Bob's CA — no warning)
  iroh Endpoint  <====== QUIC (hole-punch ======>  veld-daemon
  per-host forwarder      or n0 relay fallback)      iroh Endpoint
                                                     local TCP listener :PORT  (Caddy upstream)
                                                        registers via Helper: AddHost + AddRoute
```

- iroh `Endpoint` lives in **veld-daemon** (the only always-on process). New module
  `crates/veld-daemon/src/share/`.
- Node identity = an ed25519 keypair persisted once at
  `~/.local/share/veld/node.key`. `NodeId` (public key) is the stable address —
  this is the "shared DNS" made concrete, with no name server.
- Shared ticket/manifest **types** live in `veld-core` so both the CLI (encode/
  decode, print) and the daemon can use them.
- CLI ↔ daemon control uses the **existing axum management API** (add
  `POST /api/share`, `POST /api/join`, `POST /api/unshare/:id`,
  `GET /api/shares`). The CLI already depends on `reqwest`. Dashboard gets these
  endpoints for free.

### New core types (`veld-core`)

```rust
struct ShareManifest {
    run_id: Uuid,
    project: String,
    nodes: Vec<SharedNode>,
    created_at: i64,
    expires_at: i64,
}
struct SharedNode {
    node: String,       // node_name
    variant: String,
    hostname: String,   // LITERAL host from NodeState.url — shipped as-is
    upstream_port: u16,  // host-local port to dial (NodeState.port)
}
struct ShareTicket {
    node_id: NodeId,          // iroh public key
    relay_url: Option<Url>,   // None => iroh default (n0 public relays)
    manifest: ShareManifest,
    capability: [u8; 32],     // bearer secret; host serves only on match
}
// ShareTicket -> base32/URL-safe string for Slack/email
```

### Host flow — `veld share <run> [--nodes a,b] [--ttl 2h] [--relay URL]`

1. Load `RunState` for the named run (`state.rs`). Select nodes (default: all
   `start_server` nodes with a URL; `--nodes` to scope).
2. Build `ShareManifest` from each `NodeState` (literal `url` host + `port`).
3. `POST /api/share` to the local daemon with the manifest + TTL.
4. Daemon: ensure the iroh `Endpoint` exists; register a **share** keyed by a
   fresh `capability`. Incoming QUIC streams present `(capability, hostname)`;
   the daemon validates the capability, checks the hostname is in the shared set,
   dials `localhost:<port>`, and pipes bytes (dumbpipe-style, multiplexed over one
   endpoint).
5. Daemon returns the `ShareTicket`; CLI encodes and prints it. Alice sends it
   over any channel.

### Consumer flow — `veld join <ticket>`

1. Decode ticket → manifest + `NodeId` + relay + capability.
2. `POST /api/join`. Daemon connects to `NodeId` over iroh.
3. For each `SharedNode`: bind a local TCP listener on a port from the existing
   allocator range (`crates/veld-core/src/port.rs`, 19000–29999). Each accepted
   TCP conn opens a QUIC stream carrying `(capability, hostname)` to the host.
4. Via the Helper client: `AddHost(hostname, 127.0.0.1)` (no-op for `.localhost`)
   + `AddRoute(route_id, hostname, upstream = localhost:<local-listener-port>)`.
5. Bob opens the identical URL. His Caddy mints a cert from his CA (no warning)
   and reverse-proxies to the local listener → QUIC → Alice's daemon → her app.

### Teardown

- `veld unshare <id>` (host): drop the share + capability, close streams →
  consumer routes start returning 502.
- `veld leave <id>` (consumer): remove the Caddy routes + hosts entries + local
  listeners via the Helper.
- Both auto-expire at `expires_at`.

## CLI & interaction design

Commands must serve two operators equally: a human at a terminal/dashboard, and
a **coding agent** acting on an instruction like *"share this with my
colleague."* That dual audience drives the design — no blocking prompt without a
flag to pre-answer it, `--json` on everything, stable IDs, and meaningful exit
codes. Delivery of the ticket (Slack/email) is the agent's job, not veld's; veld
only mints and materializes.

### Commands

| Command | Role | Purpose |
|---------|------|---------|
| `veld share <run> [--nodes a,b] [--ttl 2h] [--approve first\|manual\|auto] [--as LABEL] [--relay URL] [--json]` | host | start a share, mint a ticket |
| `veld join <ticket> [--as LABEL] [--wait\|--no-wait] [--json]` | consumer | connect, materialize URLs locally |
| `veld shares [--json]` | both | list shares, pending requests, and joins |
| `veld approve <req-id> [--json]` / `veld deny <req-id>` | host | resolve a pending join (manual mode) |
| `veld unshare <share-id>` | host | stop a share; consumer routes die |
| `veld leave <join-id>` | consumer | disconnect, remove local routes |

### Scenarios

Human host (default `first`):
```
$ veld share app --name demo
✓ Share shr_2k9f started · 1 node · expires 2h · approval: first-joiner
  Send this to your colleague:
      veldticket_AAAA…e7
```
Human consumer:
```
$ veld join veldticket_AAAA…e7 --as "Bob's MacBook"
✓ Materialized 1 URL (trusted cert, this machine):
      https://app.demo.irohtest.localhost
```
Agent host (*"share this with my colleague"*):
```
$ veld share app --name demo --approve first --json
{"share_id":"shr_2k9f","ticket":"veldticket_AAAA…e7","nodes":["app"],
 "expires_at":"2026-07-02T14:00:00Z","approve":"first"}
```
Agent consumer (*"open the shared env: <ticket>"*):
```
$ veld join veldticket_AAAA…e7 --as "agent@bob" --wait --json
{"join_id":"join_9","status":"approved","urls":["https://app.demo.irohtest.localhost"]}
```

### Accept-join mechanism

Three approval **modes**, chosen by the host at share time:

- **`first` (default)** — auto-approve the first token-valid joiner, pin its
  NodeId, hold/deny the rest. Host is notified *after* ("Bob joined").
  Frictionless for humans and agents; safe because it is a single grant behind
  the token. Matches the literal common ask ("share with my colleague" — one
  person).
- **`manual`** — every join parks as a pending request; host approves *before*
  access. This is the interactive "accept dialog."
- **`auto`** — approve every token-valid joiner (multi-viewer demo). Explicit
  opt-in; least safe.

Wire flow (`manual`):

1. Consumer daemon dials the host NodeId over iroh, presenting
   `(capability token, --as label, its NodeId)`.
2. Host validates the token — **gate 1**. Invalid → rejected instantly, *no
   prompt* (prevents prompt-spam DOS).
3. Valid → host creates a pending request, **parks the connection**, fires a
   `join_request` event on the existing daemon broadcast bus
   (`crates/veld-daemon/src/broadcaster.rs`).
4. Host resolves on any surface → approve: pin NodeId, open streams;
   deny/60s-timeout: close.
5. Consumer proceeds: bind local ports, Helper `AddHost` + `AddRoute`, print URLs.

Approval **surfaces** — deliberately **no native OS popups** (that means
per-platform code: osascript/zenity/toast + LaunchAgent GUI quirks). The
management UI is the single GUI surface; the CLI is the headless/agent fallback.
Both fed by the one broadcast event:

| Surface | Role |
|---------|------|
| **Dashboard** (`veld.localhost`) | *the* approval UI — pending join request with `[Approve]`/`[Deny]`, live over the existing event stream |
| **Open browser on join request** | the push: on a valid join request the daemon opens the default browser to that request's approval page (`open`/`xdg-open`/`start`). No Notifications API, no permission grant, **no native code** — the tab appearing *is* the ping |
| **CLI** `veld shares` / `veld approve` / `veld deny` | fallback for headless hosts and coding agents (no browser to open) |

Net: **push = daemon opens the browser to the approval page; pull = CLI.** No
OS-specific code, no notification permissions. Caveat: opening a tab steals
focus — acceptable because only token-valid joiners (gate 1) can trigger it and
`manual`/`first` cap the frequency. Headless host → `open` no-ops → CLI path.

**Identity:** the iroh `NodeId` is the real cryptographic identity and is what
gets pinned on approval. `--as LABEL` is a self-asserted, **untrusted** hint
shown next to it for human readability only — never a security boundary.

**Default approval mode:** **human/GUI host → `manual`** (each join opens the
browser to the approval page; one click to approve/deny). **Agent/headless host
→ `first`** (no browser to open, pre-authorized, visible via
`veld shares --json`). Same command; the fallback is explicit, not magic.

## Security model

Stance: this is a developer tool. Sharing a link is the user's call, like
`ngrok` or a shared Google Doc. We don't try to be a zero-trust perimeter — we
give two gates that make casual sharing safe and put the human in the loop.

**Two gates, each with a distinct job:**

1. **Capability token in the ticket** — a 32-byte secret embedded in the ticket
   and persisted in `RunState` as a sensitive value (reuses the existing
   at-rest encryption for `sensitive_keys`, `crates/veld-core/src/state.rs:137`).
   The host refuses any iroh connection that doesn't present the matching token.
   Job: randoms/scanners can't even *knock* — no token, no connection, and no
   ability to spam the host with join prompts.

2. **Approve-join dialog** — when a token-valid peer connects, the host holds the
   stream **pending** and fires an event on the existing daemon broadcast bus
   (`crates/veld-daemon/src/broadcaster.rs`). The management UI
   (`crates/veld-daemon/src/management.rs`, `:19899`) shows
   *"`<NodeId>` wants to join `<run>` — [Approve] [Deny]"*. iroh authenticates
   the peer's `NodeId` (ed25519 pubkey) at connect time, so approval **pins that
   NodeId** for the session. Job: a *leaked* ticket still can't get in without a
   live human click.

Token = "allowed to knock." Approval = "come in." The human gate is the backstop
for a leaked link — which is what makes the casual-sharing stance safe rather
than naive.

**Unattended sharing:** `veld share --auto-approve` drops gate 2 (token only) for
"here's a link, look whenever, I'm offline" flows. Default is both gates.

Supporting properties:

- **Scoped to named nodes.** The host's forwarder refuses any hostname not in the
  shared manifest. Never "the whole machine," never arbitrary ports.
- **Expiry + revoke.** Every share has a TTL and an immediate `veld unshare`
  (drops the token + closes streams).
- **Stateful backends are the user's call.** A shared dev DB (one store, two
  people) will surprise you; we can't enforce read-only, so a one-time note on
  first share, no more.
- **Relay sees nothing.** Traffic is end-to-end encrypted between the two velds;
  a relay (n0's or self-hosted) forwards sealed bytes and never sees URLs or
  content.

## Naming / collision policy

Shipping literal hostnames means byte-identical URLs — which is what makes
app-emitted redirects, cookies, and CORS Just Work (no proxy rewriting). But it
collides if the consumer already runs the same env locally.

**Rule: the consumer's own local URL always wins.**

- **Default: claim the identical hostname.** Common case — the consumer is a
  viewer, not also running Alice's env — so the name is free.
- **On local clash** (consumer already owns `frontend.my-feature.proj.localhost`):
  the local route takes precedence; the incoming share is peer-scoped (insert an
  origin segment, e.g. `frontend.alice.my-feature.proj.localhost`) and we **warn**
  that same-origin app behavior (absolute redirects, cookie domain, CORS) may
  break under the rewrite. This is accepted — seamless for the viewer case,
  degrades only for the co-developer case.

Seamless by default; degrade only when forced.

## Relay tiers

- **Zero-config (v1 default):** iroh's built-in n0 public relays. No infra we own.
  Caveat: n0 documents these as dev/testing-grade.
- **Self-host:** `--relay <url>` → team's own `iroh-relay` (Docker image
  `n0computer/iroh-relay`), supports allowlist + token auth.
- **LAN:** no relay touched at all (direct / mDNS).

## Phased plan

- **Phase 0 — spike (throwaway):** in veld-daemon, one hardcoded env, one node,
  no auth. Prove: iroh endpoint + ticket + TCP forward + Helper `AddRoute` →
  consumer opens the URL with a trusted cert. Validates the whole thesis.
- **Phase 1 — MVP:** `ShareManifest`/`ShareTicket` in veld-core; `veld share` /
  `veld join` / `veld unshare` / `veld leave`; axum endpoints; multi-node
  multiplex; **both security gates** (capability token in ticket + approve-join
  dialog pinning NodeId) + TTL + `--auto-approve`; local-URL-wins collision
  fallback; n0 relays.
- **Phase 2 — hardening:** dashboard surface (share button + active-shares list +
  the approve/deny prompt on the management UI), `--relay` self-host, docs
  (README, docs/, skills, schema per the AGENTS.md checklist).
- **Phase 3 — maybe:** persistent shares across daemon restarts; live discovery
  (pkarr/DNS) so tickets survive host address changes instead of being snapshots.

## Open questions / risks (call these before building)

1. **CLI ↔ daemon control path.** Today the daemon socket is broadcast-only
   (`crates/veld-daemon/src/broadcaster.rs`). Plan adds request/response via the
   existing axum HTTP API — confirm that's acceptable vs extending the socket
   protocol.
2. **Daemon GC must count active shares as activity.** If the daemon dies, shares
   die — that is fine (fail-closed; consumer gets a clean connection error). The
   only requirement: the idle-GC path must not reap the daemon while a share is
   active with no local run running. One-line liveness check, not a redesign.
3. **Custom apex needs privileged setup on the consumer.** A shared
   `*.mycompany.dev` env only works if the consumer ran `veld setup privileged`
   (custom apex needs dnsmasq/hosts). `.localhost` works unprivileged. Document
   the constraint.
   **Same-setup-mode constraint (accepted):** both peers must be in the same
   setup mode so the hostname string matches exactly — both privileged (clean
   URL) or both unprivileged (`:18443` in the URL). Accepted as a documented
   requirement, not a blocker.
4. **Ticket is a snapshot.** Host public address can change; a static ticket goes
   stale. Fine for share-now; Phase 3 live discovery fixes persistent shares.
5. **Interactive protocols. VALIDATED END-TO-END (two machines).** A manual
   `dumbpipe`-based mock (`prosperity-solutions/veld-iroh-test-app`) ran Next.js
   16 dev on the host and joined from a second MacBook on a different network:
   hole-punched iroh tunnel, consumer Caddy route + trusted cert, **identical
   shared URL**, and **websocket HMR reloaded live on the consumer's browser**.
   The host-mismatch worry was correctly resolved by identical-hostname (consumer
   is indistinguishable from the host locally). No dev-server config injection was
   needed. Nothing architectural remains unproven.

### Design note — consumer-side readiness

The mock exposed a readiness-model issue: probing the consumer node with
`http GET /` gates on an HTTP 200 *round-tripped over the tunnel*, which is
fragile (iroh session-setup latency) and semantically wrong — the consumer runs
a tunnel, not a server. **`veld join` must define readiness as "iroh connection
established + local listener bound," not an HTTP probe through the tunnel.** (The
browser worked regardless because it doesn't consult veld's readiness state.)
6. **Latency.** Every consumer request round-trips to the host. Expected and
   acceptable for preview/review; not a production path.
7. **iroh relay grade.** n0 public relays are "dev/testing" per n0. If shared
   previews become load-bearing, graduate to self-hosted relays.

## What's genuinely new code vs reused

- **New:** iroh dependency; `veld-daemon/src/share/` (endpoint, forwarders,
  ticket serving); `ShareManifest`/`ShareTicket` in veld-core; 4 axum endpoints;
  4 CLI subcommands; node keypair persistence.
- **Reused as-is:** Helper `AddHost`/`AddRoute`/`RemoveRoute`; Caddy route
  injection; per-machine CA + cert issuance; `RunState`/`NodeState` as the
  manifest source; port allocator; tokio + axum + reqwest already in the tree.
```

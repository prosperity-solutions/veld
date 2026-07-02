# Implementation Plan: P2P environment sharing

Companion to `RFC-p2p-sharing.md`. This is the sequenced build plan, grounded in
verified file:line ground truth. Build order is strictly bottom-up
(core types → daemon transport → daemon control API → CLI → dashboard → docs)
because the layers depend on each other; it is deliberately **not** parallelized.

## Principles

- Reuse, don't reinvent: Helper `add_route`/`add_host`, `PortAllocator`, the
  `X-Veld-Request` CSRF convention, the `open`/`xdg-open` browser helper, and the
  `run(...) -> i32` + `--json` CLI shape all already exist.
- Fail-closed and simple: shares/joins are **in-memory in the daemon**. Only the
  node keypair persists. Daemon down ⇒ shares gone ⇒ consumer gets a clean
  connection error. (Accepted; no `RunState` token persistence.)
- No config-schema change — sharing is purely runtime.
- Every layer lands compiling + `clippy -D warnings` clean + unit-tested before
  the next layer starts.

## Verified ground truth (anchors)

- Workspace: edition 2024, MSRV 1.85, resolver 2, tokio `full` everywhere,
  `quinn` already transitive (`Cargo.toml`). axum 0.8 in veld-daemon.
- Daemon: `#[tokio::main]`, spawns monitor/gc/feedback_server/accept; **holds no
  in-memory app state today**; runs until signal (`veld-daemon/src/main.rs:66`).
- Control seam: extend axum HTTP on `127.0.0.1:19899`; socket is broadcast-only
  (`veld-daemon/src/broadcaster.rs`). Dashboard **polls**, no SSE
  (`management-ui.html:528`, 3s).
- Helper client: `add_host(hostname,ip)`, `add_route(json{route_id,hostname,upstream})`,
  `remove_route(route_id)`, `remove_host`, `reload_dns`
  (`veld-core/src/helper.rs:190-216`). route_id `veld-{run}-{node}-{variant}`,
  upstream `localhost:{port}` (`orchestrator.rs:1404-1406`).
- Ports: `PortAllocator::allocate() -> PortReservation` (holds a guard listener),
  range 19000-29999 (`veld-core/src/port.rs`).
- Sensitive at-rest: `sensitive::{encrypt_value,decrypt_value}` (`sensitive.rs`).
- Data dir: `dirs::data_dir().join("veld")` (`state.rs:334`).
- Dashboard: `management-ui.html` embedded via `include_str!`
  (`management.rs:18`), vanilla-JS IIFE, mutations gated by `X-Veld-Request`.
- Browser open: `open`/`xdg-open` pattern (`veld/src/commands/ui.rs:26-32`).

## Dependencies to add

- `veld-daemon/Cargo.toml`: `iroh = "1"`, `iroh-tickets = "1"` (or the ticket
  path re-exported by `iroh`), `data-encoding` (base32) or reuse `base64 0.22`.
  Pin exact versions against docs.rs at build time; verify they compile on the
  stable toolchain + edition 2024.
- `veld-core/Cargo.toml`: `iroh-tickets` only if the shared `ShareTicket` type
  needs to parse the inner iroh ticket in-core. Prefer keeping iroh entirely in
  the daemon and letting core treat the inner ticket as an opaque string.

## Module layout (new code)

```
crates/veld-core/src/share.rs        # shared types: ShareManifest, SharedNode,
                                      # ShareTicket (encode/decode), capability,
                                      # DaemonClient (reqwest -> :19899). Unit tests.
crates/veld-daemon/src/share/
    mod.rs                            # ShareManager (Arc), wired into AppState
    endpoint.rs                       # iroh Endpoint build + persistent keypair
    host.rs                           # accept loop, control handshake, per-node forward
    join.rs                           # dial, control handshake, local listeners, route inject
    forward.rs                        # bidirectional TCP<->QUIC copy (dumbpipe-style)
    api.rs                            # axum routes: /api/shares/*
crates/veld/src/commands/
    share.rs join.rs shares.rs approve.rs deny.rs unshare.rs leave.rs
crates/veld-daemon/assets/management-ui.html   # + approval panel & shares list
```

## Wire protocol

- **ALPN:** `b"veld/share/1"`.
- **Ticket:** `base64url(JSON { iroh_ticket: String, manifest: ShareManifest,
  capability: [u8;32] })`. `iroh_ticket` is iroh's own `EndpointTicket` (NodeId +
  relay + addrs); we never invent addressing.
- **Control stream (first `bi`):** consumer → host
  `{ capability, label }`; host validates capability (gate 1), resolves approval
  (gate 2), replies `{ decision: approved|denied, reason? }`. Consumer blocks on
  this reply (drives `veld join --wait`).
- **Data streams (one `bi` per proxied TCP connection):** consumer opens a bi,
  writes a length-prefixed target hostname frame, then raw bytes; host reads the
  hostname, checks it against the shared manifest (scope gate), dials
  `localhost:<that node's port>`, and runs `forward.rs`.

## Identity & state

- Keypair: load `dirs::data_dir()/veld/node.key`; generate + persist (0600) if
  absent. NodeId = public key. This is the stable "address."
- `ShareManager` (in `AppState`, `Arc`): `endpoint: OnceCell<Endpoint>`,
  `shares: Mutex<HashMap<ShareId, Share>>`, `joins: Mutex<HashMap<JoinId, Join>>`,
  `pending: Mutex<HashMap<ReqId, PendingRequest>>`. All ephemeral.
- Approval modes: `first` (auto-approve first valid joiner, pin NodeId, hold
  rest), `manual` (park + browser popup + dashboard/CLI resolve), `auto` (approve
  all valid). Default resolved by caller: CLI picks `manual` for interactive,
  `first` for `--json`/non-TTY.

## Phases

### Phase 0 — transport spike (throwaway, proves the daemon can do it)
1. Add iroh deps to veld-daemon.
2. `endpoint.rs`: build Endpoint + persistent keypair.
3. `forward.rs`: TCP<->QUIC copy loop.
4. Minimal host + join with **no auth, single hardcoded node**, behind two
   temporary axum routes `POST /api/_spike/{share,join}`.
5. Manual check on two machines: join reaches the URL via Helper `add_route`.
   Acceptance: HMR page loads over the daemon's own iroh endpoint (parity with
   the dumbpipe mock). Then delete the `_spike` routes.

### Phase 1 — core types + real transport
1. `veld-core/src/share.rs`: `ShareManifest`, `SharedNode`, `ShareTicket`
   (encode/decode + tests), capability generation, `DaemonClient`.
2. Promote host/join to the real protocol (control handshake, manifest scope,
   multi-node, capability gate). Hostname helper extracted from `NodeState.url`.
3. `ShareManager` wired into `AppState`; forwarders as tokio tasks.

### Phase 2 — control API + CLI
1. `share/api.rs`: `POST /api/shares` (start→ticket), `POST /api/shares/join`,
   `GET /api/shares`, `POST /api/shares/requests/{id}/{approve,deny}`,
   `DELETE /api/shares/{id}` (unshare), `DELETE /api/shares/joins/{id}` (leave).
   All mutations require `X-Veld-Request`; validate like `management.rs`.
2. CLI handlers (thin `reqwest` → daemon), new `Command` variants + dispatch,
   `--json`, exit codes. Ensure the daemon is running first (reuse/verify the
   existing daemon-ensure path; add one if none).

### Phase 3 — approval UX
1. `manual` mode parks the request; daemon opens the browser to the approval
   page (reuse the `ui.rs` open pattern in the daemon) and broadcasts an event.
2. `management-ui.html`: approval panel (poll `GET /api/shares`, show pending
   requests with Approve/Deny, list active shares/joins with unshare/leave).
3. `first`/`auto` non-interactive paths + `--wait/--no-wait` on join.

### Phase 4 — lifecycle & hardening
1. TTL expiry (drop share + close conns), `unshare`/`leave` teardown (Helper
   `remove_route`/`remove_host`, drop listeners, release ports).
2. NodeId pinning on approve; reject/limit prompt-spam pre-auth (gate 1 first).
3. GC audit: ensure the 10-min GC doesn't touch share routes/ports; ensure the
   daemon stays alive with an active share (it already runs until signal).
4. `--relay <url>` self-host support; default n0 relays.

### Phase 5 — docs & review
1. Docs checklist (AGENTS.md): README (features + CLI table), `docs/`,
   `skills/veld/SKILL.md` + reference, `website/llms-full.txt`. No schema change.
2. Sub-agent review rounds → fix → repeat. Draft PR. Wait for CI green
   (`fmt --check`, `clippy -D warnings`, `test --workspace`). Ask before merge.

## Non-obvious touch-points (easy to miss)

- **Daemon-ensure**: `veld share`/`join` must guarantee veld-daemon is running;
  verify the existing autostart path or add one.
- **Route_id namespace**: use `veld-join-{join_id}-{node}` so join routes never
  collide with orchestrator's `veld-{run}-{node}-{variant}` and are cleanly
  removable on leave.
- **DNS for custom apex on consumer**: `.localhost` needs no `add_host`; a custom
  apex does (+ privileged setup). Gate/warn accordingly.
- **Port binding race**: `PortReservation` holds a guard listener; follow the
  orchestrator idiom (allocate → get port → release → bind our TcpListener).
- **CSRF header**: dashboard fetch() for approve/deny must send `X-Veld-Request`.
- **Local-URL-wins collision**: on join, if the hostname already has a route in
  the consumer's Caddy/registry, peer-scope + warn (don't clobber).
- **Graceful stream teardown**: close QUIC streams on TCP EOF both directions so
  HMR websockets don't hang half-open.
- **iroh logging noise**: scope iroh's tracing so it doesn't flood `veld` logs.

## Testing

- Unit (veld-core): `ShareTicket` round-trip, manifest (de)serialize, capability
  gen/compare, hostname extraction, route_id formatting.
- Integration (veld-daemon): two in-process endpoints over the loopback relay —
  host+join, control handshake approve/deny/timeout, one forwarded TCP stream
  echoes.
- Manual: the two-MacBook rig (`veld-iroh-test-app`) but driven by real
  `veld share`/`veld join` instead of dumbpipe.

## Open decisions carried from RFC

- Default approval mode (`manual` interactive / `first` agent) — confirm with
  maintainer before Phase 3 UX polish.

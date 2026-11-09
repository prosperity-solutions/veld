---
format: 1
title: Sharing an environment
summary: "Let others open your dev server: peer-to-peer with a colleague (per-port opt-in), or as a public password-protected URL via a self-hosted veld-gateway."
aliases: [network share, veld network share, join, veld-gateway, gateway, p2p, iroh, relay, share.expose, web share, public url, expose dev server to the internet, let someone outside open my app]
tags: [sharing]
links: [gateway-guide]
related: [share-an-environment]
covers: [crates/veld-share/**, crates/veld-gateway/**, crates/veld/src/commands/share.rs, docs/gateway.md]
---
- **Peer** (`expose: ["peer"]`): an encrypted iroh tunnel to another Veld. The colleague opens the
  same URLs on their machine; a shared `tcp` port appears as a local port there.
- **Web** (`expose: ["web"]`, `veld network share --web`): a self-hosted `veld-gateway` gives a
  public URL anyone can open, password-protected by default. Only `http` ports.
- Consent is **per port**: only ports that declare `share.expose` leave your machine.
- `sharing.relays` must be set explicitly (`"public"` or your own relay URLs).
- Shares stop when the run stops or the daemon stops.

## What it is not
- No Veld-hosted server and no accounts. The gateway and private relays are yours to run
  ([gateway guide](link:gateway-guide)).

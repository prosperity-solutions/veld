---
format: 1
title: URLs, ports and HTTPS
summary: How url_template, named ports and Caddy's local CA give every service a stable https://*.localhost URL instead of a port number.
aliases: [url_template, localhost url, https, caddy, certificate, tls, port 18443, privileged, unprivileged, named ports, ports]
tags: [urls, setup]
related: [fix-certificates-and-urls, install-veld, architecture]
covers: [crates/veld-core/src/url.rs, crates/veld-core/src/port.rs, crates/veld/src/commands/setup/**, crates/veld-helper/**]
---
- `url_template` (default style `{service}.{run}.{project}.localhost`) builds each URL. Also
  `{branch}`, `{worktree}`, `{username}`, `{hostname}` and the fallback `{branch ?? run}`.
- `ports`: absent = one auto `http` port; `null` = none; a map = named ports. An `http` port gets
  a hostname and route; a `tcp` port is only allocated and exported (`VELD_PORT_<NAME>`).
  Port `19899` is the daemon's.
- Caddy terminates TLS with a local CA that setup trusts in the system keychain.
- **Privileged** setup (`veld self setup privileged`, sudo once) binds 80/443: clean URLs and
  custom apex domains. **Unprivileged** (no sudo, the default bootstrap) uses port `18443` in URLs
  and only `.localhost`.

## What it is not
- Not public: these URLs resolve to your own machine. See [sharing](entry:sharing) to show them to someone.

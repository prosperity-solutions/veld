---
format: 1
title: Share an environment with a colleague
summary: Opt ports in with share.expose, run veld network share, send the join link, approve the join; --web for a public URL via your gateway.
aliases: [share, veld network share, join, veld network join, approve, share with colleague, public url, --web, open my dev server from the internet, show my local app to someone]
tags: [sharing]
links: [gateway-guide]
related: [sharing]
covers: [crates/veld/src/commands/share.rs, crates/veld-share/**, crates/veld/src/commands/skills/sharing.md]
---
Both sides need Veld, in the same setup mode (both privileged or both unprivileged).

1. In `veld.json`, set `"sharing": { "relays": "public" }` (or your own relays) and add
   `"share": { "expose": ["peer"] }` on each port (or variant) to share.
2. Start the environment, then `veld network share <run>`. It prints a join URL and a
   `veld network join <ticket>` command.
3. Your colleague opens the join URL (or runs the command).
4. Approve in the dashboard or with `veld network approve <id>`.
5. Stop with `veld network unshare`; `veld network list` shows live shares.

For someone without Veld: add `"web"` to `expose`, configure `sharing.gateway`, and run
`veld network share --web`. See [the gateway guide](link:gateway-guide).

## What it does not do
It never shares a port that did not opt in, and stops when the run or the daemon stops.

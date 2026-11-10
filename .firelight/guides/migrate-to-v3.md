---
format: 1
title: Migrate a config to schemaVersion 3
summary: An old veld.json (schemaVersion 1 or 2) no longer loads; migrate it with docs/migrating-to-v3.md and check with veld orchestrator lint.
aliases: [schemaVersion 2, schemaVersion 1, migrate, migration, upgrade config, v3, old config]
tags: [config]
links: [migrating-to-v3]
related: [veld-json]
covers: [docs/migrating-to-v3.md, schema/v3/**]
---
1. Read the error: it lists every change your config needs.
2. Follow [migrating-to-v3.md](link:migrating-to-v3). It is written to hand to a coding agent.
3. Set `"schemaVersion": "3"` and `"$schema": "https://veld.oss.life.li/schema/v3/veld.schema.json"`.
4. Run `veld orchestrator lint` until it exits 0.

## What it does not do
Veld does not rewrite the config for you.

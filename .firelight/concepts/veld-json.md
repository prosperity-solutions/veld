---
format: 1
title: veld.json
summary: The one JSONC config file (schemaVersion 3) that declares a project's nodes, presets, URLs, setup steps and sharing.
aliases: [veld.jsonc, veld config, veld configuration, schemaVersion, include, config file, schema]
tags: [config]
links: [config-reference, json-schema, migrating-to-v3]
related: [nodes-and-variants, presets, urls-and-https, migrate-to-v3]
covers: [schema/**, crates/veld-core/src/config.rs, crates/veld-core/src/include.rs, crates/veld-core/src/jsonc.rs, crates/veld-core/src/variables.rs, docs/configuration.md]
---
`veld.json` (or `veld.jsonc`) sits in the project root. It allows comments and trailing
commas, and `include` globs split it into per-directory files.

- `"$schema": "https://veld.oss.life.li/schema/v3/veld.schema.json"` gives your editor completion.
- `schemaVersion: "3"` is required.
- `nodes` are the services; `presets`, `url_template`, `setup`/`teardown`, `env`, `vars`,
  `sharing` and `ide` sit beside them.
- Values interpolate: `${veld.port}`, `${veld.url}`, `${nodes.backend.url}`, `${vars.*}`.
- `veld orchestrator lint` checks it (exit 1 on errors); `veld orchestrator config --files`
  and `--why <pointer>` show where a value came from.

Full reference: [configuration.md](link:config-reference). `veld agent skills config` prints it too.

## What it is not
- Not read when older: a v1 or v2 config fails to load with a pointer to
  [the v3 migration](entry:migrate-to-v3).
- Not rewritten by veld. Veld never edits your config.
- Not a secret store: a value can come from an env var, file or command and be marked `secret`;
  veld keeps the pointer, not the secret.

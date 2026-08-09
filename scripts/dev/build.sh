#!/usr/bin/env bash
# Build everything the dev stack runs, before any of it starts.
#
# A `command` node rather than a step inside each long-running node, because
# three of them share it and a build that fails should fail the run before the
# daemon, the UI and Electron all start against a half-built tree.
#
# The npm half delegates to `just dev-deps` instead of repeating the guard: the
# check for "node_modules exists but predates a new dependency" has a documented
# subtlety (npm's own .package-lock.json marker), and one copy of it is enough.
set -euo pipefail

cd "${VELD_DEV_ROOT:?VELD_DEV_ROOT must be set by the veld node}"

# The daemon AND the CLI: the dev daemon spawns its sibling target/debug/veld
# for UI-triggered start/stop, and a stale sibling silently falls back to the
# installed CLI, which refuses a schema-ahead dev DB.
cargo build -p veld-daemon -p veld

just dev-deps

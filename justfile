export PATH := env("HOME") + "/.cargo/bin:" + env("PATH")

# Dedicated DEV INSTANCE for source-built binaries: tier-1 `just dev` (and
# the veld-dev wrapper / dev-daemon) run with their own database, daemon
# port, and daemon socket — never the installed veld's. Dev builds carry
# newer schema migrations, and letting one loose on the real DB would migrate
# it forward and blind the installed daemon (NewerSchema) until `veld update`.
# The dev daemon runs ALONGSIDE the installed one and serves its own
# dashboard at https://veld-dev.localhost.
# NOTE: the helper/Caddy/DNS layer is NOT instanced — it is a singleton
# owning 443/18443 and system DNS; both instances share it (distinct
# hostnames and route ids keep them apart).
dev_db := justfile_directory() + "/.veld-dev/veld.db"
# What a plain `cargo run`/`cargo test` resolves to (see `Db::cargo_target_db`).
# Separate from `dev_db` so tests never write the file a running dev daemon owns.
cargo_db := justfile_directory() + "/.veld-dev/veld-cargo.db"
# The dev instance's daemon port — installed daemon keeps 19899; both run
# side by side. Dev CLI and dev daemon must agree on this.
dev_daemon_port := "19898"
# The dev daemon's control socket — under $HOME, NOT inside the checkout, and
# that is a length bound rather than tidiness. A unix socket path is capped by
# `sockaddr_un::sun_path` (104 bytes on macOS), and a worktree under
# `~/git/_worktrees/<branch-name>/` blows through it: the failure is `bind`
# reporting "path must be shorter than SUN_LEN", which reaches you as a dev
# daemon that will not start and names nothing you can act on. This is the same
# bound `veld_core::instance::pty_dir` already moved the *holder* sockets out of
# the checkout for; the control socket was simply missed. Keyed by port, so it
# separates instances exactly as much as the port and the dashboard hostname
# already do — two worktrees cannot both be the dev instance either way.
dev_daemon_sock := env("HOME") + "/.veld/dev-" + dev_daemon_port + ".sock"
# Clear the per-node variables veld injects, for the bootstrap recipes that
# RUN something against an instance — `dev`, `dev-daemon`, `dev-ui`,
# `dev-desktop`, `dev-desktop-embedded`. The install/restore and dev-db recipes
# do not take it: they address the system install or an explicit path, and
# neither reads these. `dev-real` DOES take it — it runs the source binary
# against an instance, so it carries exactly the hazards listed below.
#
# These recipes are most useful from a terminal inside the dev stack's own /ide
# — that is the documented escape hatch — and such a terminal inherits the
# dev-daemon node's environment through the PTY holder. Left in place:
# `VELD_PORT` makes `just dev-ui` try to bind the dev daemon's port and die on
# `strictPort`, and `VELD_URL`/`VELD_PROXY_ORIGINS` make the bootstrap daemon
# trust three origins that route to a DIFFERENT run's processes — origins that
# provably do not proxy its own /api, which is the one thing that variable's
# name promises. Assignment to empty, because a recipe cannot unset a variable;
# `env_nonempty` on the Rust side and `||` in vite.config.ts both read empty as
# absent.
#
# `VELD_PTY_DIR` is here for a sharper reason than the rest: a bootstrap daemon
# that inherits it binds its holders in — and writes its `shims/` into — the
# RUNNING stack's holder directory, so two daemons with different databases
# adopt each other's terminal sessions. That is verbatim the hazard the
# root-keyed digest in `scripts/dev/daemon.sh` exists to prevent. Safe to clear
# here because no recipe using this variable sets it.
#
# `VELD_DAEMON_PORT` is deliberately NOT in this list: `dev` and `dev-daemon`
# assign it just before the prefix, and a later assignment on the same command
# line wins, so including it would blank the port those recipes exist to set.
# `dev-ui` clears it on its own line instead.
clear_stack_env := "VELD_PORT= VELD_URL= VELD_PROXY_ORIGINS= VELD_PTY_DIR="
# The INSTANCE variables, cleared for recipes that must never address whichever
# instance the surrounding terminal belongs to.
#
# `VELD_DB_PATH` is the dangerous one and the reason this exists. A terminal
# opened inside the dev stack's own /ide — including the `claude` and `codex`
# agent panes this repo ships — inherits the dev-daemon node's environment,
# because nothing calls `env_clear` between the daemon and a PTY holder. And
# `Db::path_override` consults `VELD_DB_PATH` BEFORE `cargo_target_db`, so it
# defeats the backstop whose entire job is stopping `cargo test` from writing
# "the database a running dev daemon owns". `just test` in such a pane migrated
# the live run's database. Same leak, milder symptom, for the other two: a bare
# `veld status` there silently addresses the dev instance.
#
# Separate from `clear_stack_env` on purpose: `dev` and `dev-daemon` assign
# `VELD_DB_PATH` deliberately, and a command-prefix assignment later in the line
# wins — folding these in would blank the very paths those recipes set.
clear_instance_env := "VELD_DB_PATH= VELD_DAEMON_PORT= VELD_DAEMON_SOCK= VELD_PTY_DIR="

# ============================================================================
# Veld Development Workflow
#
# THE USUAL WAY IS NOT IN THIS FILE. The whole dev stack — dev daemon, /ide with
# HMR, and the Electron shell — is a veld environment declared in the root
# veld.json:
#
#   veld start --preset dev            this worktree's stack, parallel-safe
#   veld start --preset dev-headless   the same without Electron
#   veld status / veld logs / veld stop
#
# Everything there is keyed off the run: its own allocated ports, its own
# database under .veld-dev/<run>/, its own hostnames, and its own
# ~/.local/bin/veld-dev-<run>. Two worktrees can each have one up at once.
#
# What follows is the BOOTSTRAP tier, and it is a deliberate singleton — one
# worktree at a time, on fixed ports. It exists because you need a way to run
# the daemon when the thing you broke is `veld start` itself, and because a
# first clone has nothing to start a veld run with. Reach for it in that case;
# otherwise use the preset above.
#
#   just dev <args>           CLI only, no install, own dev instance (most changes)
#   just dev-daemon           Daemon from source, alongside the installed one
#                             (own port/DB/socket, dashboard: veld-dev.localhost)
#   just dev-ui               vite for /ide on 5199, against `just dev-daemon`
#   just dev-desktop          Electron against `just dev-ui`
#   just dev-db-reset         Wipe the dev DBs (fresh state)
#   just dev-db-from-real     Snapshot the REAL DB into the dev DB (migration rehearsal)
#   just dev-install-daemon   Install daemon (overlay/feedback changes)
#   just dev-install-helper   Install helper + restart Caddy (proxy changes, sudo)
#   just dev-install          CLI + daemon (no sudo)
#   just dev-install-all      Everything including helper (sudo)
#   just dev-restore          Go back to the released version
#
# Tier 1 uses a dedicated SQLite file (.veld-dev/veld.db, gitignored); the
# install tiers replace the system binaries and operate on the real DB.
# ============================================================================

# --- Tier 1: Run CLI from source (no install, own state) ---

# Build and run veld from source against the dev DB. Does NOT touch the
# system install or its database.
# Usage: just dev start --name foo website:local
dev *ARGS:
    cargo build
    mkdir -p "{{justfile_directory()}}/.veld-dev"
    VELD_DB_PATH="{{dev_db}}" \
    VELD_DAEMON_PORT="{{dev_daemon_port}}" \
    VELD_DAEMON_SOCK="{{dev_daemon_sock}}" \
    {{clear_stack_env}} \
        ./target/debug/veld {{ARGS}}

# Run the daemon from source, foreground, ALONGSIDE the installed one — own
# DB, own port (19898), own socket, own dashboard at https://veld-dev.localhost
# (self-registered Caddy route; removed again on Ctrl-C). Gives dev-DB runs
# their monitoring/GC without touching the installed daemon.
dev-daemon:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    # Daemon AND CLI: the daemon spawns its sibling target/debug/veld for
    # UI-triggered start/stop — a stale (or missing) sibling silently falls
    # back to the installed CLI, which refuses a schema-ahead dev DB.
    cargo build -p veld-daemon -p veld
    mkdir -p .veld-dev
    # Claim the veld-dev wrapper for THIS worktree: the wrapper hardcodes a
    # worktree path (dev-link), and a wrapper pointing at another (possibly
    # deleted) worktree targets the wrong dev instance — or nothing at all.
    # Whichever worktree runs the dev daemon is the dev instance.
    mkdir -p "$HOME/.local/bin"
    printf '#!/usr/bin/env bash\nexport VELD_DB_PATH="{{dev_db}}"\nexport VELD_DAEMON_PORT="{{dev_daemon_port}}"\nexport VELD_DAEMON_SOCK="{{dev_daemon_sock}}"\nexec "{{justfile_directory()}}/target/debug/veld" "$@"\n' > "$HOME/.local/bin/veld-dev"
    chmod +x "$HOME/.local/bin/veld-dev"
    echo "✓ veld-dev wrapper → this worktree"
    echo "Dev daemon: port {{dev_daemon_port}}, DB {{dev_db}}, dashboard https://veld-dev.localhost"
    VELD_DB_PATH="{{dev_db}}" \
    VELD_DAEMON_PORT="{{dev_daemon_port}}" \
    VELD_DAEMON_SOCK="{{dev_daemon_sock}}" \
    VELD_MANAGEMENT_HOST="veld-dev.localhost" \
    {{clear_stack_env}} \
        ./target/debug/veld-daemon

# Run the source-built CLI against the REAL installed DB — for inspecting
# runs the installed veld started (e.g. feedback loops on a shared run).
# ⚠ ONLY safe when your branch adds no schema migration: a schema-ahead dev
# binary MIGRATES the real DB forward on open, and the installed veld/daemon
# then fail with NewerSchema until `veld update`. If in doubt, don't — use
# `just dev` + `just dev-daemon`, or `just dev-install`.
dev-real *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    cargo build
    # Highest schema version this branch's binary migrates to = number of
    # MIGRATIONS entries (their `version:` fields are consecutive from 1,
    # enforced by the migrations_are_consecutive test).
    branch_v=$(grep -cE '^        version: [0-9]+,' crates/veld-core/src/db/mod.rs || true)
    real="$HOME/Library/Application Support/veld/veld.db"
    if [ -f "$real" ]; then
        real_v=$(sqlite3 "$real" 'PRAGMA user_version;' 2>/dev/null || echo '?')
        if [ "$real_v" != "?" ] && [ "$branch_v" -gt "$real_v" ]; then
            echo "⚠ This branch has schema v$branch_v; your real DB is v$real_v."
            echo "  Running would migrate the REAL DB and break the installed veld."
            echo "  Aborting. Use 'just dev' (isolated) or 'just dev-install'."
            exit 1
        fi
    fi
    VELD_LIB_DIR="{{justfile_directory()}}/target/debug" \
    {{clear_stack_env}} \
        ./target/debug/veld {{ARGS}}

# Wipe the dev DB (including WAL/SHM sidecars) for a fresh-state run.
# Wipe BOTH dev databases: the `just dev` instance's, and the one every
# cargo-built binary uses (`Db::cargo_target_db`). Removing only the first leaves
# `veld-cargo.db` behind, and a database stranded at a `user_version` whose
# migration was later rewritten never gets the corrected one — every query naming
# the new column then fails. That happened during #167 §5b.
#
# Bootstrap-tier databases only. A veld-run dev stack keeps its database under
# `.veld-dev/<run>/`, which belongs to that run and is reset by naming the
# variant instead — `veld start dev-db:fresh dev-electron dev-link`. Wiping
# those from here would delete the state of a stack that is currently up.
# `just dev-db-list` shows them.
dev-db-reset:
    rm -f "{{dev_db}}" "{{dev_db}}-wal" "{{dev_db}}-shm"
    rm -f "{{cargo_db}}" "{{cargo_db}}-wal" "{{cargo_db}}-shm"
    @echo "Bootstrap dev DBs reset ({{dev_db}}, {{cargo_db}})"
    @echo "Per-run dev stack DBs are untouched — see 'just dev-db-list'."

# Every per-run dev database in this worktree, with its schema version.
# The counterpart to `veld status`: these outlive a stopped run on purpose, so
# stopping the stack does not throw away the state you were debugging.
dev-db-list:
    #!/usr/bin/env bash
    set -euo pipefail
    shopt -s nullglob
    found=0
    for db in "{{justfile_directory()}}"/.veld-dev/*/veld.db; do
        run=$(basename "$(dirname "$db")")
        echo "$run  v$(sqlite3 "$db" 'PRAGMA user_version;' 2>/dev/null || echo '?')  $db"
        found=1
    done
    [ "$found" = 1 ] || echo "No per-run dev databases yet. Start one with 'veld start --preset dev'."

# Snapshot the REAL installed DB into the dev DB — migration rehearsal:
# the next `just dev <cmd>` migrates the COPY forward while the real file
# stays untouched (and the installed daemon stays healthy). Uses sqlite3
# .backup for a consistent online copy (a plain cp can tear a WAL DB).
dev-db-from-real:
    #!/usr/bin/env bash
    set -euo pipefail
    real="$HOME/Library/Application Support/veld/veld.db"
    [ -f "$real" ] || { echo "No installed DB at $real"; exit 1; }
    mkdir -p "{{justfile_directory()}}/.veld-dev"
    rm -f "{{dev_db}}" "{{dev_db}}-wal" "{{dev_db}}-shm"
    sqlite3 "$real" ".backup '{{dev_db}}'"
    chmod 600 "{{dev_db}}"
    echo "Snapshotted real DB → {{dev_db}} (schema v$(sqlite3 "{{dev_db}}" 'PRAGMA user_version;'))"
    echo "Next 'just dev <cmd>' will migrate this copy; the real DB is untouched."

# Create a `veld-dev` wrapper in ~/.local/bin for cross-project use.
# Carries the dev DB too — veld-dev state never mixes with the installed veld's.
dev-link:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    cargo build
    mkdir -p "$HOME/.local/bin" .veld-dev
    wrapper="$HOME/.local/bin/veld-dev"
    printf '#!/usr/bin/env bash\nexport VELD_DB_PATH="{{dev_db}}"\nexport VELD_DAEMON_PORT="{{dev_daemon_port}}"\nexport VELD_DAEMON_SOCK="{{dev_daemon_sock}}"\nexec "{{justfile_directory()}}/target/debug/veld" "$@"\n' > "$wrapper"
    chmod +x "$wrapper"
    echo "Created $wrapper — use 'veld-dev' from any directory."
    echo "Remove with: rm $wrapper"

# --- Tier 2: Install daemon (user-level, no sudo) ---

# Install dev daemon and restart the service.
# Use when: you changed the feedback overlay, client-log, or daemon code.
dev-install-daemon:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    cd crates/veld-daemon/frontend && npm run build && cd ../../..
    touch crates/veld-daemon/src/main.rs
    cargo build -p veld-daemon

    # Find where the launchd plist actually points
    plist_bin=""
    for plist in "$HOME/Library/LaunchAgents/dev.veld.daemon.plist"; do
        if [ -f "$plist" ]; then
            # Anchored on the executable line, not on "veld-daemon" anywhere in the
            # file: the plist now also names ~/.veld/veld-daemon.log, and a plain
            # first-match grep would hand back the log path the moment the keys are
            # ever reordered — then `cp` and `codesign` would land on the log file.
            plist_bin=$(grep -E "<string>[^<]*/veld-daemon</string>" "$plist" | head -1 | sed 's/.*<string>//;s/<\/string>.*//' | tr -d '[:space:]')
            [ -n "$plist_bin" ] && break
        fi
    done

    # Always install to lib dir
    lib_dst="$HOME/.local/lib/veld/veld-daemon"
    cp ./target/debug/veld-daemon "$lib_dst"
    codesign -s - -f "$lib_dst" 2>/dev/null || true

    # Also copy to wherever the plist points (if different)
    if [ -n "${plist_bin:-}" ] && [ "$plist_bin" != "$lib_dst" ]; then
        echo "Plist points to $plist_bin — copying there too"
        cp ./target/debug/veld-daemon "$plist_bin"
        codesign -s - -f "$plist_bin" 2>/dev/null || true
    fi

    echo "Installed: $("$lib_dst" --version)"

    # Restart daemon service
    rm -f ~/.veld/daemon.sock
    if launchctl list dev.veld.daemon &>/dev/null; then
        launchctl kickstart -k "gui/$(id -u)/dev.veld.daemon" 2>/dev/null || true
    fi

    sleep 2
    if curl -sf http://127.0.0.1:19899/api/environments >/dev/null 2>&1; then
        echo "✓ Daemon running"
    else
        echo "✗ Daemon not responding — run 'veld doctor'"
    fi

# --- Tier 3: Install helper (privileged, requires sudo) ---

# Install dev helper and restart Caddy.
# Use when: you changed Caddy config, route building, GODEBUG, TLS, etc.
dev-install-helper:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    cargo build -p veld-helper

    # Find where the launchd plist actually points — copy there.
    plist_bin=""
    for plist in /Library/LaunchDaemons/dev.veld.helper.plist "$HOME/Library/LaunchAgents/dev.veld.helper.plist"; do
        if [ -f "$plist" ]; then
            plist_bin=$(grep "veld-helper" "$plist" | head -1 | sed 's/.*<string>//;s/<\/string>.*//' | tr -d '[:space:]')
            [ -n "$plist_bin" ] && break
        fi
    done

    # Always install to lib dir
    lib_dst="$HOME/.local/lib/veld/veld-helper"
    cp ./target/debug/veld-helper "$lib_dst"
    codesign -s - -f "$lib_dst" 2>/dev/null || true

    # Also copy to wherever the plist points (if different)
    if [ -n "${plist_bin:-}" ] && [ "$plist_bin" != "$lib_dst" ]; then
        echo "Plist points to $plist_bin — copying there too"
        cp ./target/debug/veld-helper "$plist_bin"
        codesign -s - -f "$plist_bin" 2>/dev/null || true
    fi

    echo "Installed: $("$lib_dst" --version)"

    # Restart privileged helper (prompts for sudo)
    if sudo launchctl list dev.veld.helper &>/dev/null 2>&1; then
        echo "Restarting privileged helper..."
        sudo launchctl kickstart -k "system/dev.veld.helper"
    elif launchctl list dev.veld.helper &>/dev/null; then
        echo "Restarting unprivileged helper..."
        launchctl kickstart -k "gui/$(id -u)/dev.veld.helper"
    else
        echo "⚠ No helper service found — run 'veld setup'"
        exit 1
    fi

    sleep 1
    echo "✓ Helper restarted."
    echo "  Restart your runs to pick up the new helper: veld restart --name <run>"

# --- Tier 4: Build and install Caddy with local veld_inject module ---

# Build Caddy with the local inject module and install it.
# Use when: you changed caddy/inject/*.go
dev-install-caddy:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    XCADDY_VERSION=$(cat .xcaddy-version)
    echo "Building Caddy with local veld_inject module (xcaddy $XCADDY_VERSION)..."
    xcaddy build \
        --with github.com/prosperity-solutions/veld/caddy/inject=./caddy/inject \
        --output ./target/caddy
    dst="$HOME/.local/lib/veld/caddy"
    cp ./target/caddy "$dst"
    codesign -s - -f "$dst" 2>/dev/null || true
    echo "✓ Caddy installed ($("$dst" version))"
    echo "  Restart your runs to pick up the new Caddy."

# --- Compound targets ---

# Install CLI + daemon (no sudo needed).
dev-install:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    cargo build
    mkdir -p "$HOME/.local/lib/veld"
    # CLI
    cp ./target/debug/veld "$HOME/.local/bin/veld"
    codesign -s - -f "$HOME/.local/bin/veld" 2>/dev/null || true
    echo "CLI: $(veld --version)"
    # Daemon
    just dev-install-daemon

# Install everything (CLI + daemon + helper + Caddy). Requires sudo + Go.
dev-install-all:
    just dev-install
    just dev-install-caddy
    just dev-install-helper

# Restore to the released version.
dev-restore:
    veld update

# --- Build / Test / Lint ---

build:
    cd crates/veld-daemon/frontend && npm run build
    cd crates/veld-daemon/ui && npm run build
    cargo build

test:
    {{clear_instance_env}} cargo test --workspace
    cd crates/veld-daemon/frontend && npm test
    cd crates/veld-daemon/ui && npm test
    cd desktop && npm test

lint:
    {{clear_instance_env}} cargo clippy --workspace --all-targets
    cargo fmt --all --check
    cd crates/veld-daemon/frontend && npx tsc --noEmit
    cd crates/veld-daemon/ui && npm run typecheck
    # JS/TS surfaces — a new linter is its own guard (see `.github/workflows/ci.yml`);
    # keeping it in `just lint` means a local run is as strict as a CI job.
    cd crates/veld-daemon/frontend && npm run lint
    cd crates/veld-daemon/ui && npm run lint
    cd desktop && npm run lint
    # Folded into `lint` (unlike `workflow-gates`) because it is bash and grep
    # only — no dependency to install — and the drift it catches is invisible
    # to every other check in this recipe.
    just favicon
    just topbar-height
    just shellcheck

# Parse and lint `install.sh` — the one program here that `curl | bash` runs and
# that no compiler, test or linter reads. `--severity=warning` is where the file
# sits clean; the remaining `info` diagnostics are deliberate. Mirrors the
# `schema` job's step, so a local run catches what CI would.
shellcheck:
    #!/usr/bin/env bash
    set -euo pipefail
    bash -n install.sh
    bash -n tests/validate-install-contract.sh
    if command -v shellcheck >/dev/null 2>&1; then
      shellcheck --severity=warning install.sh tests/validate-install-contract.sh scripts/dev/*.sh
    else
      echo "shellcheck not installed — skipping (brew install shellcheck). CI runs it."
    fi
    # Neither `bash -n` nor shellcheck can see the thing that actually breaks:
    # a variable renamed on one side of the CLI/install.sh boundary. Both halves
    # stay individually valid while every app update quietly reinstalls the CLI.
    ./tests/validate-install-contract.sh

# Assert every surface's inlined favicon still matches website/favicon.svg
# (docs/branding.md). Five copies across HTML, Rust, and JS, tied together by
# nothing but this gate.
favicon:
    ./tests/validate-favicon.sh

# Assert `.topbar.electron`'s CSS height and desktop/src/main.js's TOPBAR_HEIGHT
# agree. Same reasoning as `favicon`: bash and sed only, and the drift it catches
# (macOS traffic lights centred against a constant that no longer matches the bar
# they sit on) is invisible to every compiler, linter and test in the repo.
topbar-height:
    ./tests/validate-topbar-height.sh

# The two gates over .github/workflows/. Run this whenever you touch one.
#
#   1. No CI job can run on a draft PR (AGENTS.md → CI cost convention).
#   2. release.yml's publish script cannot ship an incomplete release, or
#      leave a complete one stranded as a draft — which is what
#      softprops/action-gh-release did to v16.57.1.
#
# Deliberately not folded into `lint`: these need PyYAML, and `lint` is the
# recipe every contributor runs constantly — it must not grow a Python dep.
# Install it with `python3 -m pip install --user pyyaml`.
# The `schema` job in ci.yml is the enforcing copy — and because that job is
# itself draft-guarded, this local recipe is the ONLY thing that catches either
# failure before CI has already spent a run on it.
workflow-gates:
    python3 tests/validate-workflow-gates.py --selftest
    python3 tests/validate-workflow-gates.py
    # --selftest first, same reasoning as above: it mutates the completeness
    # gate out of the publish script and asserts the suite goes red.
    python3 tests/validate-release-publish.py --selftest
    python3 tests/validate-release-publish.py

# Check commit subjects against the pattern the `commits` job enforces. That job
# is draft-guarded too, so without this a malformed subject is discovered only
# after the PR is marked ready and the expensive legs have already dispatched.
# The regex is duplicated from .github/workflows/ci.yml on purpose: extracting it
# from a YAML `run:` block is more fragile than two copies of a list of
# conventional-commit types that changes approximately never. ci.yml is the
# enforcing copy; if they disagree, ci.yml wins.
commit-subjects base="origin/main":
    #!/usr/bin/env bash
    set -euo pipefail
    pattern='^(feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert)(\(.+\))?!?: .+'
    # Command substitution, NOT `while read ... < <(git rev-list ...)`. `set -e`
    # does not reach into a process substitution, so a stale or unfetched
    # {{base}} made the loop read zero lines, leave failed=0, and exit 0 —
    # reporting "clean" for a check that never ran. An assignment carries the
    # substitution's status, so this form actually fails. (Also avoids `mapfile`,
    # which macOS's bundled bash 3.2 does not have.)
    shas=$(git rev-list "{{base}}..HEAD")
    if [ -z "$shas" ]; then
        echo "No commits in {{base}}..HEAD — nothing to check." >&2
        exit 0
    fi
    failed=0
    # Unquoted on purpose: SHAs are hex, so word-splitting is safe here.
    for sha in $shas; do
        msg=$(git log --format='%s' -1 "$sha")
        if echo "$msg" | grep -qE "$pattern"; then
            echo "✅ $msg"
        else
            echo "❌ $msg"
            failed=1
        fi
    done
    if [ "$failed" -eq 1 ]; then
        echo "Expected: type(scope)?: description — see CONTRIBUTING.md" >&2
        exit 1
    fi

# Is this worktree's branch behind origin's default branch? This repo's
# worktrees drift when `main` is not updated, and a stale branch lacks the
# latest DB migrations — so its schema tests fail and its PRs conflict late,
# exactly the failure `docs/extensions-vision.md` and the `/ship` pre-flight
# exist to catch early. Fetches first so "behind" means behind *the remote's
# current state*, not whatever was last fetched.
#
# The agentic harness (`.claude/skills/ship`) runs this before starting work,
# before the review loop, and before opening a PR; it exits nonzero when the
# branch is behind, so a gate can stop on it. Fix by
# `git fetch origin && git rebase origin/<default>` (fast-forward if unpushed).
stale-check base="origin/main":
    #!/usr/bin/env bash
    set -euo pipefail
    git fetch origin
    # Authoritative default branch: what origin says HEAD is, falling back to the
    # caller's {{base}} when the remote does not advertise it.
    default=$(git ls-remote --symref origin HEAD 2>/dev/null | awk '$1=="ref:" {print $2}' | sed 's#refs/heads/##') || true
    if [ -z "$default" ]; then
        default={{base}}
        echo "origin does not advertise a default branch — using $default" >&2
    fi
    # Commits in origin/<default> not in HEAD — how far *behind* the branch is.
    # (`origin/$default..HEAD` would be the reverse: commits in HEAD not in
    # origin, which is 0 exactly when the branch is behind, the case to catch.)
    behind=$(git rev-list --count "HEAD..origin/$default")
    echo "HEAD is $behind commit(s) behind origin/$default" >&2
    if [ "$behind" -gt 0 ]; then
        echo "⚠  Branch is behind origin/$default — update main before working (git fetch origin && git rebase origin/$default)" >&2
        exit 1
    fi

build-frontend:
    cd crates/veld-daemon/frontend && npm run build

test-frontend:
    cd crates/veld-daemon/frontend && npm test

lint-frontend:
    cd crates/veld-daemon/frontend && npx tsc --noEmit
    cd crates/veld-daemon/frontend && npm run lint

setup-frontend:
    cd crates/veld-daemon/frontend && npm install

# --- Management UI v2 (crates/veld-daemon/ui) + desktop shell (desktop/) ---

# npm deps for ui/, installed only when they are missing.
#
# A dependency of every ui/ recipe rather than something to remember: a fresh
# worktree has no node_modules (they are per checkout, not shared), and the
# failure is a bare "vitest: command not found" that says nothing about which
# setup step was skipped. `just setup-ui` stays as the explicit, always-runs
# version — use it after a dependency bump.
[private]
ui-deps:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}/crates/veld-daemon/ui"
    # Presence is not enough — npm records the tree it installed in
    # node_modules/.package-lock.json, so a checkout that predates a *new*
    # dependency has a complete-looking node_modules that is missing it, and the
    # failure is a cryptic "Cannot find package". `-nt` is also true when the
    # marker is absent, which is the interrupted-install case (`npm ci` removes
    # node_modules first). Same rule as crates/veld-daemon/build.rs.
    if [ ! -d node_modules ] || [ package-lock.json -nt node_modules/.package-lock.json ]; then
        npm install
    fi

# The same for desktop/, plus the Electron binary.
#
# `npm install` alone is not enough: the binary is fetched by electron's install
# script, and npm defers install scripts it has not been told to allow (`npm
# approve-scripts`), which leaves a complete node_modules whose `electron` cannot
# run — `sh: electron: command not found`. `install-electron` is the bin electron
# ships for exactly this, and `path.txt` is the marker it writes when the download
# landed, so this is a no-op on every later run.
[private]
desktop-deps:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}/desktop"
    if [ ! -d node_modules ] || [ package-lock.json -nt node_modules/.package-lock.json ]; then
        npm install
    fi
    if [ ! -f node_modules/electron/path.txt ]; then
        echo "Fetching the Electron binary (npm deferred its install script)…"
        ./node_modules/.bin/install-electron
    fi

# Everything npm the dev stack needs, guarded so it is a no-op once installed.
# Public because `scripts/dev/build.sh` (the `dev-build` node) calls it — the
# guard for "node_modules exists but predates a new dependency" has a subtlety
# worth having exactly one copy of.
dev-deps: ui-deps desktop-deps

build-ui: ui-deps
    cd crates/veld-daemon/ui && npm run build

test-ui: ui-deps
    cd crates/veld-daemon/ui && npm test

lint-ui: ui-deps
    cd crates/veld-daemon/ui && npm run typecheck
    cd crates/veld-daemon/ui && npm run lint

# Install/refresh every npm dep the UI and the desktop shell need. Unlike the
# guarded checks above this always runs npm, so it also picks up a bump.
setup-ui:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}/crates/veld-daemon/ui"
    npm install
    cd "{{justfile_directory()}}/desktop"
    npm install
    # Unconditionally, unlike `desktop-deps`: after an electron bump the previous
    # binary is still on disk with its marker file, and only electron's own
    # installer knows that version is now the wrong one.
    ./node_modules/.bin/install-electron

# Regenerate desktop/assets/ (app icon + menu-bar icon) from the Veld mark.
# Pure stdlib Python — nothing to install, same bytes on every machine. Rarely
# needed: the assets are committed, so run it when the brand changes.
desktop-icons:
    python3 desktop/scripts/make-icons.py

# Package Veld Desktop for this machine's OS into desktop/dist/ (electron-builder).
#
# The same commands release CI runs, minus the version injection — a local build
# is whatever `desktop/package.json` says, which between releases is 0.0.0. macOS
# artifacts are ad-hoc signed only (no Developer ID yet), so a build handed to
# someone else needs `xattr -dr com.apple.quarantine` on the other end. On Linux
# everything lands in desktop/dist/, including the .deb — which is built by a
# second invocation into desktop/dist-deb/ and moved, for the reason in
# desktop/electron-builder.yml.
desktop-package: desktop-deps
    cd desktop && npm run package:{{ if os() == "macos" { "mac" } else { "linux" } }}

# Vite dev server for the /ide UI (HMR). Proxies /api — including the terminal
# WebSocket upgrade — to the DEV daemon (port {{dev_daemon_port}}); start
# `just dev-daemon` first.
#
# Pointing this at the INSTALLED daemon (VELD_DAEMON_PORT=19899) leaves the rest
# of the UI working but breaks terminals: the daemon only trusts vite's origin
# when it is a dev instance, so the installed one on the default port refuses the
# upgrade (see `allowed_origins` in crates/veld-daemon/src/pty.rs). Deliberate —
# a dev server must not be able to open a shell through the installed daemon.
dev-ui: ui-deps
    # VELD_DAEMON_PORT too: inherited from a stack pane it would proxy /api to
    # the RUN's dev daemon rather than the `just dev-daemon` on 19898 this
    # recipe tells you to start — pointing the bootstrap tier at the very
    # daemon it exists to work around.
    cd crates/veld-daemon/ui && {{clear_stack_env}} VELD_DAEMON_PORT= npm run dev

# Electron shell pointed at the vite dev server (start `just dev-ui` first).
dev-desktop: desktop-deps
    cd desktop && {{clear_stack_env}} VELD_DESKTOP_URL=http://localhost:5199 npm start

# Electron shell straight at the dev daemon's embedded /ide (no HMR) —
# start `just dev-daemon` first.
dev-desktop-embedded: desktop-deps
    cd desktop && {{clear_stack_env}} VELD_DESKTOP_URL=http://127.0.0.1:{{dev_daemon_port}} npm start

# Electron shell against the installed daemon's embedded /ide.
desktop: desktop-deps
    cd desktop && npm start

# --- Licenses ---

# Regenerate THIRD-PARTY-LICENSES.md from the current dependency tree.
# Requires cargo-about 0.9.1 (pinned in ci.yml so output matches the CI
# drift check): `cargo install cargo-about@0.9.1 --all-features`.
# CI fails if the committed file drifts from this output, so run it and
# commit the result whenever Cargo.lock changes.
# `tr -d '\r'` normalizes CRLF that some upstream license texts carry, so the
# committed file is pure LF and the CI drift diff is byte-stable across OSes.
licenses:
    cargo about generate about.hbs | tr -d '\r' > THIRD-PARTY-LICENSES.md

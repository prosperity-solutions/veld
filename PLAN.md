# Plan: Zero-Sudo Install with Unprivileged/Privileged Modes

## Philosophy

Adoption trumps ceremony. A developer who tries Veld with `:8443` and likes it will
run `veld setup privileged`. A developer who hits sudo on `curl | bash` never tries
it at all.

## User Flows

### Flow 1: Zero-touch trial (zero sudo)

```
curl -fsSL https://veld.oss.life.li/get | bash
# → installs to ~/.local/bin + ~/.local/lib/veld (incl. Caddy)
# → no sudo, no setup required

cd my-project && veld init
veld start frontend:local
# → auto-bootstraps helper (user process, port 8443)
# → https://frontend.my-run.my-project.localhost:8443
# → first-run hint: "Tip: `veld setup privileged` for clean URLs without :8443"
```

### Flow 2: Explicit setup

```
veld setup unprivileged       # no sudo: Caddy, daemon, CA trust, helper on 8443
veld setup privileged         # sudo once: system daemon, ports 80/443, clean URLs
veld setup hammerspoon        # optional: menu bar widget
veld setup                    # shows current status
```

### Flow 3: Upgrade from existing system install

```
curl -fsSL https://veld.oss.life.li/get | bash
# → detects existing install at /usr/local/, updates in-place
# → next `veld setup privileged` rewrites LaunchDaemon plist with new paths
```

## Architecture: Two Modes

|                    | Unprivileged (default)              | Privileged (`veld setup privileged`)  |
|--------------------|-------------------------------------|---------------------------------------|
| Helper runs as     | User process (LaunchAgent/bg proc)  | System daemon (LaunchDaemon/systemd)  |
| Helper socket      | `~/.veld/helper.sock`               | `/var/run/veld-helper.sock`           |
| Caddy ports        | 8080 / 8443                         | 80 / 443                              |
| URLs               | `https://...localhost:8443`         | `https://...localhost`                |
| Sudo required      | No                                  | One-time                              |
| DNS (.localhost)    | RFC 6761 (automatic)                | RFC 6761 (automatic)                  |
| CA trust           | Login keychain (no sudo on macOS)   | Login keychain (no sudo on macOS)     |

## Command Design

```
veld setup                    # show status: current mode, what's available
veld setup unprivileged       # run unprivileged setup (no sudo)
veld setup privileged         # run privileged setup (sudo once)
veld setup hammerspoon        # install Hammerspoon Spoon (optional)
```

### `veld setup` (no args) — Status

Shows current setup state and available actions:

```
Veld Setup

  Mode:       unprivileged
  Helper:     running (user process, port 8443)
  Caddy:      installed
  CA:         trusted (login keychain)

  Available:
    veld setup privileged      Ports 80/443, clean URLs (one-time sudo)
    veld setup hammerspoon     Menu bar widget
```

### `veld setup unprivileged` — No sudo

1. Install Caddy to `~/.local/lib/veld/caddy` (download if missing)
2. Start veld-daemon as user LaunchAgent / systemd user service
3. Start veld-helper as user LaunchAgent / systemd user service (socket at `~/.veld/helper.sock`, ports 8443/8080)
4. Start Caddy via helper
5. Trust Caddy CA in login keychain (macOS) or user cert store
6. Write `~/.veld/setup.json`: `{"mode": "unprivileged"}`

### `veld setup privileged` — Sudo once

1. Self-escalate to sudo (pass resolved binary paths as args to avoid lib_dir confusion)
2. Stop user-level helper if running (connect to `~/.veld/helper.sock` → send `shutdown`)
3. Install veld-helper as system LaunchDaemon / systemd system service
   - Binary path: resolved before sudo, baked into plist (e.g., `~/.local/lib/veld/veld-helper`)
   - No `--https-port`/`--http-port` args → defaults to 443/80
4. Start Caddy via system helper (ports 80/443)
5. Trust Caddy CA (if not already trusted)
6. Write `~/.veld/setup.json`: `{"mode": "privileged"}`

### `veld setup hammerspoon` — Optional

Same as current step 6. Does not require sudo. Can be run anytime.

## Implementation Work Items

### WI-1: `install.sh` — Default to user paths, download Caddy

**Changes:**
- Default `INSTALL_DIR="$HOME/.local/bin"`, `LIB_DIR="$HOME/.local/lib/veld"`
- No sudo prompt (remove lines 139-155)
- Add Caddy download step after binary install (same logic as current `install_caddy()`)
- Detect if `~/.local/bin` is on PATH → in interactive mode, offer to append to shell rc
- Existing `/usr/local/` installs: detect via `command -v veld`, update in-place
- Don't auto-run `veld setup`
- End message: "Run `veld start` in any project to get going"
- Handle service restart for existing installs (detect mode from `~/.veld/setup.json`)

**Files:** `install.sh`

### WI-2: `paths.rs` — User-first, never create system dir

**Changes:**
```rust
pub fn lib_dir() -> PathBuf {
    // User dir first (new default)
    let user_dir = dirs::home_dir().map(|h| h.join(".local/lib/veld"));
    if let Some(ref ud) = user_dir {
        if ud.exists() { return ud.clone(); }
    }
    // System dir (existing installs only)
    let system_dir = PathBuf::from("/usr/local/lib/veld");
    if system_dir.exists() { return system_dir; }
    // Default: user dir — never try to create system dir
    user_dir.unwrap_or(system_dir)
}
```

Remove the `create_dir_all("/usr/local/lib/veld")` attempt.

**Files:** `crates/veld-core/src/paths.rs`

### WI-3: `veld-helper` — Port parameterization

**Changes:**
- Add `--https-port <PORT>` and `--http-port <PORT>` CLI args (defaults: 443/80)
- Pass ports through to `CaddyManager::start()` and `build_base_config()`
- `build_base_config(https_port, http_port)` replaces hardcoded `:443`/`:80`
- `status` response includes `{"https_port": N, "http_port": N}`

**Files:** `crates/veld-helper/src/main.rs`, `crates/veld-helper/src/caddy.rs`, `crates/veld-helper/src/handler.rs`

### WI-4: Helper protocol — Add `shutdown` command

**Changes:**
- New command `shutdown`: sends SIGTERM to Caddy, cleans up socket, exits
- Used by `veld setup privileged` to stop user-level helper before starting system helper
- Used by `veld uninstall`

**Files:** `crates/veld-helper/src/handler.rs`, `crates/veld-helper/src/protocol.rs`, `crates/veld-core/src/helper.rs`

### WI-5: `helper.rs` (core client) — Socket fallback + timeout

**Changes:**
```rust
pub async fn connect() -> Result<HelperClient> {
    // 1. Try system socket (privileged mode)
    if let Ok(client) = try_connect_timeout(&system_socket_path(), 2s).await {
        return Ok(client);
    }
    // 2. Try user socket (unprivileged mode)
    if let Ok(client) = try_connect_timeout(&user_socket_path(), 2s).await {
        return Ok(client);
    }
    Err(HelperNotRunning)
}

fn system_socket_path() -> PathBuf { /* /var/run/veld-helper.sock or /run/... */ }
fn user_socket_path() -> PathBuf { /* ~/.veld/helper.sock */ }
```

Add connect+read timeout (5s) to `send()` to prevent hanging on wedged helper.

**Files:** `crates/veld-core/src/helper.rs`

### WI-6: Caddy sentinel — Verify it's our Caddy

**Changes:**
- In `build_base_config()`, add a sentinel route with `@id: veld-sentinel`
- `is_running()` checks `GET http://localhost:2019/id/veld-sentinel`:
  - 200 → our Caddy
  - 404 → foreign Caddy (bail with clear error)
  - Connection refused → not running

**Files:** `crates/veld-helper/src/caddy.rs`

### WI-7: Auto-bootstrap in orchestrator

**Changes:**
- Replace `require_setup()` gate with `ensure_helper()`:

```rust
async fn ensure_helper() -> Result<HelperClient> {
    // Try existing helpers
    if let Ok(client) = HelperClient::connect().await {
        return Ok(client);
    }

    // Auto-bootstrap with flock to prevent races
    let _lock = flock("~/.veld/bootstrap.lock")?;

    // Re-check after lock (another process may have bootstrapped)
    if let Ok(client) = HelperClient::connect().await {
        return Ok(client);
    }

    // Caddy must be installed (by install.sh)
    if !paths::caddy_bin().exists() {
        bail!("Caddy not found. Run `veld setup unprivileged` to install.");
    }

    // Spawn helper with unprivileged ports
    spawn_user_helper(8443, 8080)?;
    wait_for_socket(&user_socket_path(), 10s)?;

    // Ensure CA is trusted
    ensure_ca_trusted().await?;

    HelperClient::connect().await
}
```

- Remove hard "Run `veld setup` first" errors from start, stop, urls, etc.

**Files:** `crates/veld-core/src/orchestrator.rs`, `crates/veld-core/src/setup.rs`, `crates/veld/src/commands/mod.rs`

### WI-8: `orchestrator.rs` — Port-aware URL construction

**Changes:**
- Query helper for `https_port` via status response
- Conditional port suffix:

```rust
let https_port = helper_status.https_port;
let https_url = if https_port == 443 {
    format!("https://{node_url}")
} else {
    format!("https://{node_url}:{https_port}")
};
```

- `${nodes.backend.url}` and `VELD_URL` env var include port when not 443

**Files:** `crates/veld-core/src/orchestrator.rs`

### WI-9: `veld setup` command — Subcommand structure

**Changes:**
```rust
#[derive(Parser)]
struct SetupArgs {
    #[command(subcommand)]
    command: Option<SetupCommand>,
}

#[derive(Subcommand)]
enum SetupCommand {
    /// No-sudo setup: Caddy, daemon, helper on port 8443
    Unprivileged,
    /// One-time sudo: system daemon, ports 80/443, clean URLs
    Privileged,
    /// Install Hammerspoon menu bar widget
    Hammerspoon,
}

// None → show status
// Unprivileged → run unprivileged setup
// Privileged → run privileged setup
// Hammerspoon → install spoon
```

**Files:** `crates/veld/src/main.rs`, `crates/veld/src/commands/setup.rs` (split into `setup/mod.rs`, `setup/unprivileged.rs`, `setup/privileged.rs`, `setup/hammerspoon.rs`, `setup/status.rs`)

### WI-10: `setup/unprivileged.rs` — The no-sudo setup

**Changes:**
1. Check ports 8080, 8443, 2019 (parameterized `check_ports()`)
2. Install Caddy if missing (download to `~/.local/lib/veld/caddy`)
3. Install daemon as user LaunchAgent (direct `launchctl bootstrap gui/<uid>`, no sudo gymnastics)
4. Install helper as user LaunchAgent with `--socket-path ~/.veld/helper.sock --https-port 8443 --http-port 8080`
5. Start Caddy via helper
6. Trust CA in login keychain
7. Write `~/.veld/setup.json`: `{"mode": "unprivileged"}`

Remove all `resolve_real_user_macos()` / `SUDO_USER` / chown logic from this path.

**Files:** `crates/veld-core/src/setup.rs`, new `crates/veld/src/commands/setup/unprivileged.rs`

### WI-11: `setup/privileged.rs` — The one sudo step

**Changes:**
1. Resolve binary paths BEFORE sudo escalation (e.g., `which_self("veld-helper")`)
2. Self-escalate: `sudo veld setup privileged --helper-bin <resolved_path> --caddy-bin <resolved_path>`
3. Stop user-level helper (connect to `~/.veld/helper.sock` → send `shutdown`)
4. Remove user-level helper LaunchAgent
5. Check ports 80, 443, 2019
6. Install Caddy if not present at resolved path
7. Install helper as system LaunchDaemon (plist references resolved binary path)
8. Start Caddy via system helper (ports 80/443)
9. Trust CA
10. Write `~/.veld/setup.json`: `{"mode": "privileged"}`

**Files:** new `crates/veld/src/commands/setup/privileged.rs`, `crates/veld-core/src/setup.rs`

### WI-12: Hint system

**Changes:**
- State in `~/.veld/hints.json`: `{"setup_privileged_count": 0}`
- On `veld start` when helper is on port != 443:
  - Count 0: full multi-line message explaining `veld setup privileged`
  - Count 1-4: single line: "Tip: `veld setup privileged` for URLs without :8443"
  - Count 5+: silent
- `veld setup` (status) always shows available upgrades regardless of hint count

**Files:** new hint module in `crates/veld/src/hints.rs`, `crates/veld/src/commands/start.rs`

### WI-13: `veld update` — Mode-aware

**Changes:**
- Read `~/.veld/setup.json` to determine current mode
- Run `install.sh` with `VELD_NON_INTERACTIVE=1` (downloads new binaries + Caddy)
- If mode is `privileged`: run `sudo veld setup privileged` to restart system services
- If mode is `unprivileged` or auto: restart user-level helper without sudo
- If no mode set (auto-bootstrapped): just restart the helper process

**Files:** `crates/veld/src/commands/update.rs`

### WI-14: `veld uninstall` — Handle both modes

**Changes:**
- Read `~/.veld/setup.json` for current mode
- Unprivileged cleanup: user LaunchAgent for helper + daemon, `~/.local/lib/veld/`, `~/.veld/`
- Privileged cleanup: escalate to sudo, remove system LaunchDaemon, `/var/run/veld-helper.sock`
- Both: remove CA from keychain, remove Hammerspoon Spoon
- Clean up old system install at `/usr/local/lib/veld/` if present

**Files:** `crates/veld/src/commands/uninstall.rs`, `crates/veld-core/src/setup.rs`

### WI-15: `version.rs` — Fix search order

**Changes:**
- Check `~/.local/lib/veld/` first, then `/usr/local/lib/veld/`

**Files:** `crates/veld/src/commands/version.rs`

### WI-16: `check_ports()` — Parameterize

**Changes:**
- `check_ports(https_port: u16, http_port: u16)` instead of hardcoded 80/443
- Unprivileged: check 8443, 8080, 2019
- Privileged: check 443, 80, 2019

**Files:** `crates/veld-core/src/setup.rs`

### WI-17: Socket permissions

**Changes:**
- System socket (`/var/run/veld-helper.sock`): keep `0o777` (CLI runs as user, needs to connect)
- User socket (`~/.veld/helper.sock`): use `0o700` (only owner needs access)

**Files:** `crates/veld-helper/src/main.rs`

### WI-18: Migration from existing system installs

**Changes:**
- On `veld setup unprivileged` or auto-bootstrap: if `/usr/local/lib/veld/caddy-data` exists and `~/.local/lib/veld/caddy-data` doesn't → copy + chown (preserves CA)
- Print: "Migrated Caddy data from system install"
- Don't remove old system files (user might want to `veld setup privileged` later)

**Files:** `crates/veld-core/src/setup.rs`

### WI-19: Docs and marketing

**Changes:**

| File | Change |
|------|--------|
| `README.md` | Zero-sudo install. Update paths, requirements, architecture section. Document `veld setup unprivileged` and `privileged`. |
| `PRD.md` | Document two-mode architecture. Remove "privileged helper" as the only option. |
| `website/llms-full.txt` | Mirror README changes |
| `website/` HTML | Update install section, getting started |
| `docs/configuration.md` | Note about `:8443` in unprivileged mode |
| CLI help text | Subcommand descriptions for setup |
| `CONTRIBUTING.md` | Update dev setup instructions |

### WI-20: Integration tests

**Changes:**
- Test unprivileged mode (no sudo required)
- Test auto-bootstrap flow
- Test transition from unprivileged → privileged
- Update CI workflow to test without sudo by default

**Files:** `tests/integration.sh`, `.github/workflows/ci.yml`

## Risk Mitigations

| Risk | Mitigation |
|------|------------|
| Concurrent bootstrap race | `flock` on `~/.veld/bootstrap.lock` |
| Foreign Caddy on port 2019 | Sentinel route `veld-sentinel` check |
| Two helpers on mode switch | `privileged` sends `shutdown` to user helper first |
| Stale socket file | Real connect with timeout, not file existence check |
| `lib_dir()` root/user mismatch | Resolved paths passed as args during sudo escalation |
| Caddy missing on first `veld start` | `install.sh` downloads Caddy; auto-bootstrap fails with clear message if missing |
| `~/.local/bin` not on PATH | Install script appends to shell rc (interactive) |
| setcap lost on Linux binary update | Re-apply in `veld update` when mode is privileged |
| pfctl rules cleared by macOS update | Detect on `veld start` — if privileged mode but port 443 unreachable, warn user to re-run `veld setup privileged` |
| Route loss on helper crash | Future follow-up: persist routes to disk |

## Out of Scope

- Route persistence across helper restarts (follow-up PR)
- Caddy admin API authentication (pre-existing issue, separate PR)
- Non-`.localhost` domain support in unprivileged mode (requires root for DNS)
- Windows support

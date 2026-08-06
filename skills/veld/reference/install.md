# Installing Veld

## Quick Install

```bash
curl -fsSL https://veld.oss.life.li/install.sh | sh
```

This installs the `veld` binary, `veld-daemon`, `veld-helper`, and Caddy to `~/.local/bin` and `~/.local/lib/veld/`.

## Post-Install Setup

After installing, run setup to configure HTTPS and the background services:

```bash
# Unprivileged mode — no sudo, uses port 18443 for HTTPS
veld setup unprivileged

# OR: Privileged mode — one-time sudo, uses port 443 for clean URLs
veld setup privileged
```

## Verify Installation

```bash
veld doctor
```

## Veld Desktop (macOS app)

```bash
veld desktop install     # install it
veld desktop status      # where it is, and whether it matches the CLI
veld desktop update      # bring it to this CLI's version
```

Installing it through the CLI rather than downloading the `.dmg` is what avoids
the Gatekeeper prompt: a browser marks a download with `com.apple.quarantine` and
macOS then refuses the first launch of an app that is not notarized, while curl
sets no such flag. The archive is checksum-verified against the release either
way. macOS only — on Linux the AppImage updates itself and a `.deb` belongs to
the package manager.

## Updating

```bash
veld update
```

This downloads the latest release and restarts the background services
(helper + daemon) onto the new binaries automatically. If Veld Desktop is
installed, it is updated too — but never installed by an update that did not find
it already there. **Running environments
are left running** — state lives in a migrated SQLite DB, so a binary swap no
longer risks stale state, and services keep serving throughout. In privileged
mode the root helper is restarted via sudo (you may be prompted once for your
password); if sudo isn't available, the helper restarts itself shortly after.

## Uninstalling

```bash
veld uninstall
```

## Requirements

- macOS (arm64 or x86_64) or Linux (x86_64)
- No root access required for unprivileged mode
- `~/.local/bin` must be in your PATH (the installer will tell you if it isn't)

## Troubleshooting

- **"command not found: veld"** — add `~/.local/bin` to your PATH: `export PATH="$HOME/.local/bin:$PATH"` (add to your shell profile)
- **"Version mismatch detected"** — run `veld update` to sync all binaries
- **HTTPS certificate warnings** — run `veld setup unprivileged` (or `privileged`) to trust the local CA
- **Port conflicts** — veld uses ports 18080/18443 (unprivileged) or 80/443 (privileged) and 19000-29999 for services
- **Where the service logs are** — macOS: the daemon logs to `~/.veld/veld-daemon.log` (owner-only; in the user's directory because launchd will not run a user agent whose log file it cannot create, and a `/usr/local` lib dir is root-owned), and the *privileged* helper to `~/.local/lib/veld/veld-helper.log`. The unprivileged helper has no log file. Linux: `journalctl --user -u veld-daemon`. `veld doctor` prints the daemon's log location under `Installation`, read from the service definition itself; "not captured" means the install predates it — `veld setup unprivileged` (or `privileged`) rewrites the definition

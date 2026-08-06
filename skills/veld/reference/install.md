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

Installed by default alongside the CLI — the two are halves of one release. Set
`VELD_DESKTOP=0` before the install script to skip it (CI boxes, servers).

```bash
veld desktop status      # where it is, and whether it matches the CLI
veld desktop install     # get it on a machine that skipped it
veld desktop update      # bring it to this CLI's version
```

Arriving through the CLI rather than a `.dmg` download is what avoids the
Gatekeeper prompt: a browser marks a download with `com.apple.quarantine` and
macOS then refuses the first launch of an app that is not notarized, while curl
sets no such flag. macOS only — on Linux the AppImage updates itself and a `.deb`
belongs to the package manager.

**An app operation never touches the CLI.** `veld desktop install|update` runs
the installer with `VELD_DESKTOP_ONLY=1`, which skips the CLI tarball, the binary
swap, the service restarts and the sudo negotiation entirely — so updating the
app cannot restart your daemon or ask for a password.

| Variable | Effect |
|---|---|
| `VELD_DESKTOP=0` | Skip the app. The opt-out for a CI box or a server |
| `VELD_DESKTOP_ONLY=1` | Install *only* the app: no CLI, no services, no sudo. macOS only |
| `VELD_DESKTOP_DIR=<dir>` | Where the app lives. When set it is the **only** location consulted, by the installer and by `veld desktop status` |

The app's own *Check for Updates…* takes the same route: it spawns
`veld desktop update`, quits so its bundle can be replaced, and the CLI reopens
it. Because nothing is watching a terminal on that path, the installer's output
goes to `~/.veld/desktop-update.log` and the outcome to
`~/.veld/desktop-update.json`, which the app reads when it comes back — a failed
update reaches you as a dialog with the reason, not as an app that quietly
reopened on the old version.

## Updating

```bash
veld update
```

This downloads the latest release and restarts the background services
(helper + daemon) onto the new binaries automatically. On macOS it moves Veld
Desktop to the same version as well, installing it if this machine has none —
`VELD_DESKTOP=0` opts out. **Running environments
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

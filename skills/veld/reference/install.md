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

## Updating

```bash
veld update
```

This downloads the latest release and restarts background services automatically.

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

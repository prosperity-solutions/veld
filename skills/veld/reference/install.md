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

On Linux `veld desktop status` therefore **reports rather than manages**: it
names the `.deb`'s binary if it finds one and says the version belongs to your
package manager. Finding nothing is not the same as nothing being installed —
an AppImage lives wherever you saved it — so it says that too, and the `--json`
output omits `installed` entirely rather than asserting `false`.

**`veld desktop install|update` never touches the CLI.** It runs the installer
with `VELD_DESKTOP_ONLY=1`, which skips the CLI tarball, the binary swap, the
service restarts and the sudo negotiation entirely — so *that command* cannot
restart your daemon or ask for a password. Note the scope: it is a property of
these two subcommands, not of "updating the app". The app's own updater goes
through `veld update` and deliberately does move everything (see below).

| Variable | Effect |
|---|---|
| `VELD_DESKTOP=0` | Skip the app. The opt-out for a CI box or a server |
| `VELD_DESKTOP_ONLY=1` | Install *only* the app: no CLI, no services, no sudo. macOS only |
| `VELD_DESKTOP_DIR=<dir>` | Where the app lives. When set it is the **only** location consulted, by the installer and by `veld desktop status` |
| `VELD_BINARY_ICONS=0` | Leave the CLI/daemon/helper with the generic executable icon (see below) |

**`veld uninstall` removes the app too**, and `veld doctor` reports it — where it
is, which version, and whether it matches the CLI.

**If the app cannot find a CLI that understands `veld desktop`**, it falls back
to pointing you at the release page rather than handing over to a CLI that would
exit with an unknown-subcommand error after the app had already quit. Update the
CLI (`curl -fsSL https://veld.oss.life.li/get | bash`) and the in-app updater
starts working again.

**The binaries get the app's icon.** On macOS the installer copies the installed
app's `.icns` onto `veld`, `veld-daemon` and `veld-helper`, because an
authorization prompt raised on their behalf — 1Password's "Allow veld-daemon to
get CLI access", a sudo sheet — otherwise shows a generic `exec` tile, i.e. asks
the user to approve access to their secrets on behalf of something they cannot
identify. The icon comes from the installed app, so a machine with
`VELD_DESKTOP=0` simply skips this; `VELD_BINARY_ICONS=0` skips it explicitly.
This runs on a full install (`curl | bash` or `veld update`), not on
`veld desktop install|update` — those are app-only by design and must not touch
binaries a root helper is among. A machine that installed the app on its own
therefore keeps the generic icon until its next `veld update`.

One documented cost: a custom icon lives in the file's resource fork, and
`codesign --verify --strict` rejects a Mach-O carrying one. Plain
`codesign --verify` passes, the ad-hoc signature is intact, and the binaries run
and are launched by launchd normally.

**The app's own *Check for Updates…* updates the whole release.** It offers
*veld `<version>`* rather than a new app, spawns `veld update --wait-pid <pid>
--relaunch --app-path <exe>` detached, quits so its bundle can be replaced, and
the CLI moves every half and reopens it. One click, one restart, no follow-up
trip to a terminal.

Which command it spawns is a **capability** decision, not a version comparison:
`veld desktop status --json` carries a `capabilities` array, and the app uses the
full route only when it contains `full-update-handoff`. A CLI without it gets the
app-only `veld desktop update --version <v>` (which needs the explicit version, or
an older CLI reinstalls its own and re-offers the newer one forever), and the
dialog says the CLI half still needs `veld update`. A CLI with no `veld desktop`
at all is not handed anything — the app points at the release page instead.

**A CLI under `/usr/local` withholds the capability on purpose**, however new it
is. `install.sh` treats that as a system install and will not relocate it — a
privileged LaunchDaemon still references `/usr/local` paths — so under
`VELD_NON_INTERACTIVE=1` it requires `sudo -n` and exits 1 when that fails. The
handoff is a detached child with no controlling terminal, so `sudo -n` fails there
unless a credential is already cached. Advertising a capability the binary cannot
deliver would turn a working app-only update into a failed full one, so those
machines keep the app-only route from the GUI and use `veld update` in a terminal
— where sudo can prompt — to move both halves. Deliberately *not* probed with
`sudo -n`: this runs on the app's six-hourly update check, and a status command
must not poke sudo on a timer.

Because nothing is watching a terminal on that path, everything the handed-off
update prints goes to `~/.veld/desktop-update.log` and the outcome to
`~/.veld/desktop-update.json`, which the app reads when it comes back — a failed
update reaches you as a dialog with the reason, not as an app that quietly
reopened on the old version.

That report carries a `half` field, `"app"` or `"release"`, because the retry
advice depends on it: a full handoff can fail on the *CLI* half and never reach
the app, and telling that user to run `veld desktop update` would move the app
while leaving the daemon on the release that actually broke. A report with no
`half` is read as `"app"` — the only thing that could have written one before the
field existed.

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

**If Veld Desktop is open it is closed first, and only with your agreement.** Its
bundle cannot be replaced while it runs, so before anything is installed the
command asks `Close Veld Desktop, update both halves, and reopen it? [Y/n]`,
reassuring you that terminal sessions belong to the daemon, keep running while
the app is closed, and reattach with their scrollback. Answering yes quits it
over an Apple Event (so `before-quit` persists the window layout, exactly as ⌘Q
would), falling back to `SIGTERM` and never `SIGKILL`, then reopens it when the
update is done — including when the update failed. Answering no updates the CLI
half only. An app that will not quit (an unanswered dialog) is left alone.

**Agents and scripts: this never closes the app behind your back.** The prompt is
reached only with a TTY on stdin *and* stdout and `VELD_NON_INTERACTIVE` unset or
empty. Anywhere else the app half is skipped with a message rather than assumed,
and EOF on stdin counts as "no" — an answer nobody gave is not consent.

There is deliberately no flag to answer it in advance, and piping in a `y` does
not work either: a pipe is not a TTY, so that run is non-interactive and skips
the app rather than reading the input. A script that wants the app updated should
**quit the app itself** and then run `veld update` — with nothing running from the
bundle there is no question to ask, and the app half proceeds normally.

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

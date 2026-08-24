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

**Optional, and the answer is remembered.** Some people use veld as an
orchestrator and never open the IDE; some use it as their IDE. The installer asks
once, records the answer in `~/.veld/desktop.json`, and every install and update
after that obeys it — so an orchestrator-only machine stops paying a ~113 MB
download on every `veld update`.

```bash
veld desktop status         # where it is, whether it matches the CLI, and the recorded answer
veld desktop install        # add it — and record "yes"
veld desktop uninstall      # remove it — and record "no". Works with no app installed
veld desktop update         # bring it to this CLI's version
```

Three states, not two: yes, no, and **never asked**. The third is where every
machine that installed veld before the app became optional starts, with the app
on disk and no answer on record. Both installers handle it the same way:

| State | Interactive run | Nobody to ask (agent, CI, the app's own handoff) |
|---|---|---|
| yes | install / keep in step | install / keep in step |
| no | never installed, never updated | never installed, never updated |
| never asked, app installed | asks once, then obeys the answer | keeps it in step |
| never asked, no app | asks once, then obeys the answer | **skips it** — no download |

The two asymmetries in that table are deliberate. A machine that *has* the app is
running the IDE, so a run that cannot ask keeps it up to date rather than
stranding it on an old version. A machine that has never had it has been managing
without one, so a run that cannot ask does not decide by downloading it.

**`VELD_DESKTOP=0` is a per-run override, not an answer.** It skips the app for
that invocation (CI boxes, servers) and deliberately records nothing — an
environment variable is a statement about one command, while the preference is
something a user can be asked about and change. Use `veld desktop uninstall` to
say "never" durably.

**Answering "no" while the app is installed removes it**, on the spot, on the run
where the answer was given. A *stored* "no" never removes anything later: if the
app reappears — a `.dmg` dragged to `/Applications` — `veld update` leaves it
alone and says so, because a recent manual install is a better signal than an old
answer, and silently deleting an app is not something an update may do.

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
| `VELD_DESKTOP=0` | Skip the app **for this run**. Records no preference — see above |
| `VELD_DESKTOP_ONLY=1` | Install *only* the app: no CLI, no services, no sudo. macOS only |
| `VELD_DESKTOP_DIR=<dir>` | Where the app lives. When set it is the **only** location consulted, by the installer and by `veld desktop status` |
| `VELD_BINARY_ICONS=0` | Leave the CLI/daemon/helper with the generic executable icon (see below) |

**`veld uninstall` removes the app too**, and `veld doctor` reports it — where it
is, which version, whether it matches the CLI, and whether you opted out (which
is why an app can be legitimately stale). `veld desktop status --json` carries the
same answer as `preference`: `"wanted"`, `"unwanted"`, or `null` for never asked.

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
*veld `<version>`* rather than a new app, spawns `veld update --target-version
<version> --wait-pid <pid> --relaunch --app-path <exe>` detached (plus
`--console`, when the CLI advertises it — see below), quits so its bundle can be
replaced, and the CLI moves every half and reopens it. One click, one restart, no
follow-up trip to a terminal.

**`--console` re-runs the update in a terminal window**, which fixes two things a
detached child cannot do. It has no surface to show progress on once the app has
quit — one to four minutes of nothing — and no controlling terminal, so on a
privileged install `sudo` can only ever be tried as `sudo -n` and fails silently
when no credential is cached. The CLI writes `~/.veld/update-console.command`
(macOS, 0700) or `~/.veld/update-console.sh` (Linux) and opens it: on macOS with
a bare `open`, so LaunchServices routes it to whatever the user registered for
`.command` — Terminal.app unless they chose otherwise — falling back to
`open -a Terminal`; on Linux through `$VELD_TERMINAL`, `$TERMINAL`,
`x-terminal-emulator`, then a list of emulators, and never at all without
`DISPLAY`/`WAYLAND_DISPLAY`.

A launcher exiting 0 is **not** evidence a window opened — `open` and every Linux
emulator are fire-and-forget. So the outer process waits up to 20s for the update
lock to be claimed by a different pid, which only a `veld update` that really
started can do, and runs the update itself (headless, as before) when that never
happens. `VELD_UPDATE_ORIGIN=console` is exported into the window so the run knows what it
is. Two paths read it: a console run that finds the lock already held stays
silent rather than writing a failure report over the parent's success, and the
handshake only accepts a holder whose origin is `console`.

**`--console` is gated on its own capability, `console-handoff`**, and not on
`full-update-handoff`. The two are genuinely independent: `veld desktop update`
moves the app half *alone*, so a new app can be driving an old CLI — one that has
always had `--wait-pid`/`--relaunch` and therefore advertises the full handoff,
while its clap rejects `--console` with a usage error and a non-zero exit. That
would happen *after* the app quit and with no report written, so the user would
reopen on the old version having been told nothing. Unlike its neighbour,
`console-handoff` is advertised unconditionally: it is a claim about this
binary's vocabulary, not about whether the machine can finish an unattended
update, and the flag degrades to a headless run on its own.

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
unless a credential is already cached. `--console` would give sudo a terminal to
prompt in, and it is still not enough to advertise the capability: the terminal is
best-effort and the headless fallback is the path that cannot finish on these
machines. Advertising a capability the binary can only *sometimes*
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

**Only one update runs at a time.** `veld update` takes a lock at
`~/.veld/update.lock` (a directory — `mkdir` is the create-or-fail primitive) and
publishes `{pid, origin, version, started_at, phase, phase_at, tty}` into
`state.json` inside
it. A second `veld update` refuses with **exit 75** (`EX_TEMPFAIL`, so an agent
can tell "retry shortly" from a real failure) and names the holder and its phase.
So does every other veld command except a small allow-list — `update`, `doctor`,
`version`, `config`, `lint`, `init`, `desktop status`, and the internal log sinks
that running environments depend on — because the rest exec binaries that are being replaced
or talk to services that are being restarted. Veld Desktop reads the same file at
startup and quits with an explanation rather than opening over its own bundle
swap.

A lock is written off — and stolen by the next `acquire` — when **either** the
holder's pid is gone, or its `phase_at` is more than 30 minutes old. Both
conditions are needed: a liveness check cannot see a run wedged at an unanswered
`sudo` prompt, and a timeout alone cannot tell a crash from a slow download.
`veld update --force` skips the wait; `veld update --status [--json]` reports the
current state without installing anything, and `veld doctor` prints the same
thing at the top of its report (deliberately not as a check, since an update in
flight is not a failure — but it does explain why the versions below it disagree).

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
- **Where the service logs are** — macOS: the daemon logs to `~/.veld/veld-daemon.log` (owner-only; in the user's directory because launchd will not run a user agent whose log file it cannot create, and a `/usr/local` lib dir is root-owned), and the *privileged* helper to `~/.local/lib/veld/veld-helper.log`. The unprivileged helper has no log file. **Caddy** logs to `~/.local/lib/veld/caddy-data/caddy.log` (rolling, 0644) — in its data directory rather than beside the other logs because in privileged mode Caddy is root and that directory is root-owned, and it is the only place certificate issuance and renewal are ever reported. Linux: `journalctl --user -u veld-daemon`. `veld doctor` prints the daemon's log location under `Installation`, read from the service definition itself; "not captured" means the install predates it — `veld setup unprivileged` (or `privileged`) rewrites the definition

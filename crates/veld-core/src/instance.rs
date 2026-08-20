//! Per-instance overrides, so a source-built dev stack (CLI + daemon) can run
//! ALONGSIDE the installed veld instead of replacing or parking it.
//!
//! An "instance" is: a database (`VELD_DB_PATH`, handled in [`crate::db`]),
//! a daemon HTTP port (`VELD_DAEMON_PORT`), a daemon Unix socket
//! (`VELD_DAEMON_SOCK`), and optionally its own management hostname
//! (`VELD_MANAGEMENT_HOST`, e.g. `veld-dev.localhost` — the daemon
//! self-registers a Caddy route for it at startup). All default to the
//! installed instance's values, so a plain environment is byte-for-byte the
//! behavior veld always had.
//!
//! The helper/Caddy/DNS layer is deliberately NOT instanced — it is a
//! singleton owning ports 80/443/18443 and system DNS; every instance shares
//! it. Only the *management* route is instance-scoped (`veld-mgmt-<host>`).
//! RUN routes are keyed by hostname ([`crate::url::run_route_id`]), so two
//! instances collide in shared Caddy exactly when they mint the same
//! hostname — which two checkouts of one repo do whenever their run names
//! match (last-write-wins, and stopping one removes the route the other still
//! needs). `veld start` refuses that case *within* one instance by checking
//! the registry, but instances have separate databases and cannot see each
//! other's runs, so nothing detects it across them. Keep dev-instance run
//! names distinct from the installed instance's.

use std::path::PathBuf;

/// The installed instance's daemon HTTP port (management UI, feedback,
/// client-logs, share control API).
pub const DEFAULT_DAEMON_PORT: u16 = 19899;

/// Hostname the installed instance's dashboard is served on, through Caddy.
///
/// Reserved: [`management_host`] refuses it, so a dev instance cannot
/// self-register the route and hijack it. The helper's base Caddy config uses
/// this same constant for the route itself (`veld-helper/src/caddy.rs`) — change
/// it here and both halves move together, which they must: a hostname the daemon
/// does not know is a hostname whose every WebSocket upgrade it refuses.
pub const MANAGEMENT_HOST: &str = "veld.localhost";

/// Ports Caddy serves on without root: `veld setup unprivileged`, and the
/// auto-bootstrap.
///
/// **The one definition point**, because three places have to agree and two of
/// them cannot see each other: `veld setup unprivileged` and the auto-bootstrap
/// tell the helper which ports to listen on, and the daemon's `Origin` allowlist
/// (`veld-daemon/src/pty.rs`, `management_ports`) decides which origins that
/// makes real. They were four independent string literals; changing the setup's
/// pair would have silently refused every terminal and every IDE channel on a
/// no-sudo install, with the allowlist's own test still green because it pinned
/// its copy against itself.
pub const UNPRIVILEGED_HTTPS_PORT: u16 = 18443;
/// See [`UNPRIVILEGED_HTTPS_PORT`].
pub const UNPRIVILEGED_HTTP_PORT: u16 = 18080;

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// Daemon HTTP port: `VELD_DAEMON_PORT` or the default. An unparseable value
/// falls back to the default rather than erroring — the CLI must keep working
/// in a polluted environment.
pub fn daemon_port() -> u16 {
    env_nonempty("VELD_DAEMON_PORT")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_DAEMON_PORT)
}

/// Whether a daemon port was **named** rather than defaulted.
///
/// The difference matters to a caller deciding whether the daemon it can reach is
/// really its own. Defaulting to [`DEFAULT_DAEMON_PORT`] finds whatever installed
/// daemon happens to be running, which is a different process from a dev
/// instance's — and by this repo's own convention an instance that sets this also
/// sets `VELD_DB_PATH`, so a named port is the signal that the pairing is
/// deliberate.
pub fn daemon_port_is_explicit() -> bool {
    env_nonempty("VELD_DAEMON_PORT")
        .and_then(|v| v.parse::<u16>().ok())
        .is_some()
}

/// Base URL of this instance's daemon control API.
pub fn daemon_base() -> String {
    format!("http://127.0.0.1:{}", daemon_port())
}

/// Upstream (`host:port`) baked into Caddy routes for feedback/client-log
/// traffic — runs started by a dev-instance CLI route their overlay traffic
/// to the dev daemon.
pub fn daemon_upstream() -> String {
    format!("localhost:{}", daemon_port())
}

/// Hostname local files are served on, for *this* instance.
///
/// **A distinct host, not a path under the dashboard**, and that is the whole point
/// of the function: a path would share an origin with the management API, which
/// would put agent-authored HTML same-origin with every route under `/api`. See
/// `veld-daemon/src/files.rs`.
///
/// `.localhost` names need no DNS entry (RFC 6761), so this costs one Caddy route
/// and nothing else.
///
/// # Why the port is in the name
///
/// The obvious spelling — `files.` in front of [`management_host`], falling back to
/// [`MANAGEMENT_HOST`] — is wrong here, and running it caught it. This repo's own
/// `dev-daemon` node **deliberately does not set** `VELD_MANAGEMENT_HOST` (veld
/// already routes that node, and two route ids for one hostname has no defined
/// winner), so every dev instance would fall through to `files.veld.localhost` and
/// register it under a shared route id — pointing the *installed* daemon's file host
/// at a dev daemon that stops existing when you stop the run.
///
/// So the instance's own port is the disambiguator, and it is omitted only on the
/// default: the installed instance keeps the clean `files.veld.localhost`, and a dev
/// instance gets `files-19001.veld.localhost`, which cannot collide with it or with
/// another dev instance.
pub fn files_host() -> String {
    let base = management_host().unwrap_or_else(|| MANAGEMENT_HOST.to_owned());
    let port = daemon_port();
    if port == DEFAULT_DAEMON_PORT {
        format!("files.{base}")
    } else {
        format!("files-{port}.{base}")
    }
}

/// The loopback port the file origin listens on, for *this* instance.
///
/// # Why a fixed port rather than an ephemeral one
///
/// The first version bound `127.0.0.1:0` and let Caddy's route hold the upstream, on
/// the reasoning that the *public* URL is what has to stay stable. That is true and it
/// is still not safe: the helper **persists** its routes and replays them after a
/// Caddy restart or a reboot (`veld-helper/src/caddy.rs`), and nothing deregisters
/// this one when the daemon dies without a clean shutdown. A stale route whose
/// upstream is a recycled ephemeral port hands `https://files.veld.localhost` — with
/// Veld's trusted certificate — to whichever local process next binds that number, and
/// a restored file pane loads from it without a click.
///
/// A fixed port cannot be recycled that way: it is derived from the instance's daemon
/// port, and [`crate::port`] excludes it from the allocator, so nothing veld starts can
/// take it either. The cost is that an unrelated process squatting it disables file
/// serving for that instance — which is loud (the daemon logs it and every caller
/// degrades to the system opener) rather than silent and cross-wired.
/// **Below [`crate::port::PORT_RANGE_START`], deliberately.** The first attempt at a
/// fixed port used `daemon_port() + 1` and broke the dev stack on the first restart:
/// the port allocator runs in the **CLI's** process, where `daemon_port()` is the
/// installed instance's, so a dev daemon's neighbour port is invisible to it — and it
/// had already handed 19002 to a node. Reserving a number the allocator cannot see is
/// not reserving it.
///
/// So the file listener lives in a band the allocator never touches at all (it starts
/// at 19000), keyed off the instance's own port so two instances differ. A collision
/// now needs two daemon ports congruent modulo 1000, and it fails loudly at bind.
pub fn files_port() -> u16 {
    17000 + (daemon_port() % 1000)
}

/// The Caddy route id this instance's file host is registered under.
///
/// Port-keyed for the same reason [`files_host`] is: a shared id means whichever
/// daemon started last owns the route, and the loser is usually the installed one
/// that a person is actually using.
pub fn files_route_id() -> String {
    format!("veld-files-{}", daemon_port())
}

/// Daemon Unix socket path: `VELD_DAEMON_SOCK` or `~/.veld/daemon.sock`.
pub fn daemon_socket() -> PathBuf {
    if let Some(p) = env_nonempty("VELD_DAEMON_SOCK") {
        return PathBuf::from(p);
    }
    default_daemon_socket()
}

/// The **installed** instance's daemon socket, ignoring `VELD_DAEMON_SOCK`.
///
/// The socket the installed daemon is on whatever this process was pointed at —
/// which is the question to ask when the thing being acted on is the installed
/// *database*, since the two overrides are independent. `veld backup restore` is
/// the caller: a CLI told to use a different socket must still notice the installed
/// daemon holding the file it is about to replace.
pub fn default_daemon_socket() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".veld")
        .join("daemon.sock")
}

/// Whether [`daemon_socket`] is the installed instance's socket rather than one
/// this process was pointed at.
///
/// Exists so a caller can ask whether the daemon it can reach is the one that owns
/// the database it is about to act on. Those are separate overrides —
/// `VELD_DB_PATH` and `VELD_DAEMON_SOCK` — and when only one of them is set the
/// socket and the database belong to different instances. `veld backup restore`
/// needs exactly that: refusing because *a* daemon is running, when that daemon
/// holds a different database entirely, blocks a restore for no reason.
pub fn daemon_socket_is_default() -> bool {
    env_nonempty("VELD_DAEMON_SOCK").is_none()
}

/// Where this instance's terminal runtime state lives: the holder sockets, and the
/// [`shim_dir`] subdirectory of executables a terminal's environment points at.
///
/// One owner for the name, because three crates need it: the daemon binds and
/// scans it, `veld doctor` reports it, and `veld uninstall` sweeps it.
///
/// Keyed by daemon **port**, not just by the socket directory: a dev instance must
/// never adopt the installed instance's terminal sessions, whose `worktree_id`s
/// come from a different database entirely. `VELD_PTY_DIR` overrides it outright,
/// which is how the daemon's own recovery test points a child at a temp dir.
///
/// **Under the home directory, not beside the daemon socket**, and that is a
/// length constraint rather than a preference. A unix socket path is capped by
/// `sockaddr_un::sun_path` — 104 bytes on macOS, 108 on Linux — and the port
/// already separates instances, so following `VELD_DAEMON_SOCK` here bought
/// nothing and cost the bound: `just dev-daemon` puts that socket inside the
/// checkout, and a checkout under `~/git/_worktrees/<branch-name>/` produced a
/// 112-byte path. The failure is `bind` reporting "path must be shorter than
/// SUN_LEN", which reaches the user as "could not open a terminal" and names
/// nothing they can act on. `socket_for` in the daemon already digests the
/// *file* name for this reason; this is the other half of the same bound.
pub fn pty_dir() -> PathBuf {
    if let Some(dir) = env_nonempty("VELD_PTY_DIR") {
        return PathBuf::from(dir);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".veld")
        .join(format!("{PTY_DIR_PREFIX}{}", daemon_port()))
}

/// The longest a holder socket path may be, and what to do about it.
///
/// `sun_path` is 104 bytes on macOS and 108 on Linux; 104 is the safe floor.
/// Checked before binding so the diagnostic can name the escape hatch — the
/// bare `bind` error says only "path must be shorter than SUN_LEN", which is
/// true, unhelpful, and surfaces as a terminal that will not open.
pub const MAX_SOCKET_PATH: usize = 104;

/// The length of `path`, if it does not fit in a `sockaddr_un`.
///
/// The one owner of the *bound*. Each caller phrases its own diagnostic, because
/// the useful half of the message is the escape hatch and there is a different one
/// per socket — `VELD_PTY_DIR` for a holder, `VELD_DAEMON_SOCK` for the daemon's
/// control socket.
pub fn socket_path_over_limit(path: &std::path::Path) -> Option<usize> {
    let len = path.as_os_str().as_encoded_bytes().len();
    (len >= MAX_SOCKET_PATH).then_some(len)
}

/// Whether `path` fits in a `sockaddr_un`, with the message to show when it
/// does not.
pub fn socket_path_too_long(path: &std::path::Path) -> Option<String> {
    let len = socket_path_over_limit(path)?;
    Some(format!(
        "the terminal holder's socket path is {len} bytes, over the {MAX_SOCKET_PATH}-byte \
         limit a unix socket allows ({}). Set VELD_PTY_DIR to a shorter directory.",
        path.display()
    ))
}

/// The prefix every [`pty_dir`] shares, for code that must find the holder
/// directories of **every** instance rather than only this one — `veld uninstall`,
/// which has to stop them all.
pub const PTY_DIR_PREFIX: &str = "pty-";

/// Every holder socket in `dir`, and nothing else that happens to live there.
///
/// **The chokepoint, and the reason it exists rather than each caller writing
/// its own `read_dir`:** three of them already do — the daemon's adoption sweep,
/// `veld uninstall`'s hangup sweep and `veld doctor`'s count — and every one of
/// them then *acts* on what it found, destructively. The fourth such scan is the
/// one somebody writes next, and the natural version of it filters on `.sock`,
/// which is how `veld doctor` came to connect to every socket in a directory
/// `VELD_PTY_DIR` can point anywhere. Reach for this instead of `read_dir`.
///
/// A missing directory is an empty list: it appears with the first terminal and
/// nothing prunes it, so "not there" is the ordinary state, not an error.
pub fn holder_sockets_in(dir: &std::path::Path) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        // Anything else — permissions, a broken volume — is not the ordinary
        // "no terminals yet" state, and the caller's answer to it (adopt nothing,
        // hang up nothing, report nothing) looks identical to a clean run. The
        // daemon's adoption sweep used to say this itself; it says it here now so
        // all three scans do.
        Err(e) => {
            tracing::warn!("could not read the terminal holder directory {dir:?}: {e}");
            return Vec::new();
        }
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_holder_socket_name(path))
        .collect()
}

/// Whether a path has the exact shape the daemon gives a holder socket: a
/// 16-hex-digit lowercase digest of the session id, plus `.sock`.
///
/// A name check, not a claim about what is behind the name — the greeting is what
/// proves a socket is the session it should be. `VELD_PTY_DIR` is a plain
/// environment variable, so a scan that trusts the `.sock` extension alone will
/// happily act on `daemon.sock` if somebody points it one level up at `~/.veld`.
///
/// Prefer [`holder_sockets_in`] — this is the predicate behind it, exposed for
/// the callers that already hold a path.
pub fn is_holder_socket_name(path: &std::path::Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".sock"))
        .is_some_and(|stem| {
            stem.len() == 16
                && stem
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
        })
}

/// Where this instance keeps the little executables it puts in a terminal's
/// environment — today `veld-open`, plus `open`/`xdg-open` wrappers (see
/// `veld-daemon/src/pty/shims.rs`).
///
/// **Inside [`pty_dir`], which is what makes it follow `VELD_PTY_DIR`.** These
/// scripts point a URL at *this* daemon's session registry, so they are per-instance
/// exactly like the holder sockets beside them — and being under the same override is
/// not a detail: keyed off the daemon port under the real `~/.veld` instead, the
/// integration test that spawns a daemon on an ephemeral port (`pty_recovery.rs`,
/// which redirects every other piece of state) wrote a directory into the
/// developer's home on every run.
///
/// A subdirectory is invisible to the holder scans, which require a `.sock`
/// extension *and* a digest-shaped name (`is_holder_socket_name`) — but that is the
/// contract this relies on, so do not loosen those to "everything in the directory".
///
/// `VELD_SHIM_DIR` is the variable a terminal sees, and it is an *output* of this
/// function rather than an input — a client-supplied directory here would be a
/// client-supplied executable on a developer's PATH.
pub fn shim_dir() -> PathBuf {
    pty_dir().join("shims")
}

/// Management hostname this daemon should self-register with the helper
/// (e.g. `veld-dev.localhost`). `None` for the installed instance — its
/// `veld.localhost` route is part of the helper's base Caddy config.
///
/// Rejected (returns `None`, with a warning): a value that isn't a plausible
/// hostname, and `veld.localhost` itself — self-registering the installed
/// dashboard's hostname would hijack it to this instance (last route wins).
pub fn management_host() -> Option<String> {
    let host = env_nonempty("VELD_MANAGEMENT_HOST")?;
    let valid = !host.is_empty()
        && host.len() <= 253
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        && host != MANAGEMENT_HOST;
    if !valid {
        tracing::warn!(
            host,
            "ignoring VELD_MANAGEMENT_HOST: not a valid hostname (or it is \
             the installed dashboard's veld.localhost)"
        );
        return None;
    }
    Some(host)
}

/// Extra browser origins a **dev** daemon may accept a terminal upgrade from,
/// and the empty list for the installed one.
///
/// The gate is inside this function on purpose. The trust decision is "am I the
/// installed daemon?", and a caller that has to remember to ask separately is a
/// caller that eventually forgets — so the installed instance cannot obtain
/// these values at all, whatever its environment says.
///
/// Two sources, and the difference between them is the whole design:
///
/// - **`VELD_URL`** — veld injects a long-running node's own public URL into its
///   environment (`orchestrator.rs`), so a daemon started *as a veld node* is
///   handed the origin it is reached at. Nothing needs to declare it, and there
///   is no second place for it to be wrong.
/// - **`VELD_PROXY_ORIGINS`** — origins that same-origin-**proxy** this daemon's
///   `/api`, comma-separated. That is the narrow thing a vite dev server is: the
///   browser's `Origin` is vite's, and the upgrade arrives here through vite's
///   proxy. The name says the invariant, because the invariant is what makes an
///   entry safe — `mint_ticket`'s `X-Veld-Request` check means an origin that
///   does *not* proxy `/api` cannot obtain a ticket in the first place, and a
///   list called "trusted origins" invites entries that quietly rely on that.
///
/// Every value is normalised and exact-matched. Wildcards are deliberately
/// unsupported: prefix and suffix matching is how origin checks get bypassed.
pub fn dev_trusted_origins() -> Vec<String> {
    if daemon_port() == DEFAULT_DAEMON_PORT {
        return Vec::new();
    }
    let mut out = Vec::new();
    if let Some(raw) = env_nonempty("VELD_URL") {
        match normalize_origin(&raw) {
            Some(origin) => out.push(origin),
            // veld itself wrote this, so a rejection means the URL shape changed
            // and the terminal allowlist silently lost the daemon's own origin.
            None => tracing::warn!(url = raw, "ignoring VELD_URL: not a bare origin"),
        }
    }
    if let Some(raw) = env_nonempty("VELD_PROXY_ORIGINS") {
        for entry in raw.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            match normalize_origin(entry) {
                Some(origin) => out.push(origin),
                None => tracing::warn!(
                    origin = entry,
                    "ignoring VELD_PROXY_ORIGINS entry: not a bare origin \
                     (scheme://host[:port], no path, no wildcard)"
                ),
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// `scheme://host[:port]` with an optional trailing slash, or `None`.
///
/// Deliberately strict rather than lenient. This is the value an exact
/// comparison against a browser's `Origin` header is made from, so anything it
/// accepts and normalises differently to the browser is a rule that never
/// matches — and anything it accepts *loosely* is a widened gate. A wildcard, a
/// path, a query, or credentials are all rejected outright.
fn normalize_origin(raw: &str) -> Option<String> {
    // At most ONE trailing slash: `https://host/` is how an origin-shaped URL is
    // commonly written, but `https://host//` is a path and must not be trimmed
    // into looking like one.
    // Lowercased up front, not at the end: a browser serialises scheme and host
    // lowercase, and an origin has no other part — so there is nothing here that
    // case could belong to, and the scheme match below has to see it folded.
    let raw = raw.trim().to_ascii_lowercase();
    let raw = raw.strip_suffix('/').unwrap_or(&raw);
    let (scheme, rest) = raw.split_once("://")?;
    if !matches!(scheme, "http" | "https") {
        return None;
    }
    if rest.is_empty() || rest.contains(['/', '?', '#', '@', '*', ' ']) {
        return None;
    }
    // A port, if present, must be a port — `host:` and `host:80x` are neither a
    // hostname nor an origin, and a browser would never send them. A bracketed
    // IPv6 literal with no port is all colons and has nothing to split on, so it
    // is recognised before the split rather than failing it.
    let host = if rest.starts_with('[') && rest.ends_with(']') {
        rest
    } else {
        match rest.rsplit_once(':') {
            Some((host, port)) => {
                // The port must be a port AND be spelled the way a browser
                // spells it. `u16::from_str` also accepts `+80` and `080`, and
                // an origin carrying a default port (`http://h:80`) is written
                // without it — so all three would be stored as entries the
                // exact match at the call site can never hit. A rejection is
                // logged and a dead entry is not, which makes the loose version
                // strictly worse than no entry: the symptom is a 403 on a
                // WebSocket handshake, whose reason a browser cannot show.
                let parsed = port.parse::<u16>().ok()?;
                if parsed.to_string() != port {
                    return None;
                }
                let default_port = if scheme == "https" { 443 } else { 80 };
                if parsed == default_port {
                    return None;
                }
                host
            }
            None => rest,
        }
    };
    if host.is_empty()
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '[' | ']' | ':'))
    {
        return None;
    }
    Some(format!("{scheme}://{rest}"))
}

#[cfg(test)]
mod tests {
    /// A dev instance must never claim the installed instance's file host or route id.
    ///
    /// The naive spelling — `files.` in front of the management host, falling back to
    /// the constant — was written, and running it caught this: a dev daemon registered
    /// `files.veld.localhost` under a shared route id and pointed the *installed*
    /// daemon's file host at a process that stops existing when the run stops. The port
    /// in the name is what prevents that, and it is only omitted on the default.
    #[test]
    fn the_file_host_and_route_are_per_instance() {
        // `files_host`/`files_route_id` read the environment, so drive them through the
        // same accessors rather than mutating a global: the default port is the
        // installed instance, and any other port is somebody else.
        let installed_host = format!("files.{MANAGEMENT_HOST}");
        let dev_host = format!("files-19001.{MANAGEMENT_HOST}");
        assert_ne!(installed_host, dev_host);
        assert_eq!(
            format!("veld-files-{DEFAULT_DAEMON_PORT}"),
            format!("veld-files-{DEFAULT_DAEMON_PORT}")
        );
        assert_ne!(
            format!("veld-files-{DEFAULT_DAEMON_PORT}"),
            "veld-files-19001",
            "a dev instance's route id must differ from the installed one's"
        );

        // And the live functions agree with that shape for whatever instance this test
        // process is: the host always starts `files.` or `files-<port>.`, and the route
        // id always carries this instance's port.
        let host = files_host();
        assert!(
            host.starts_with("files.") || host.starts_with("files-"),
            "{host}"
        );
        assert!(host.ends_with(&format!(".{MANAGEMENT_HOST}")) || management_host().is_some());
        assert_eq!(files_route_id(), format!("veld-files-{}", daemon_port()));
        // The clean name belongs to the default port and nothing else.
        assert_eq!(
            files_host() == format!("files.{MANAGEMENT_HOST}"),
            daemon_port() == DEFAULT_DAEMON_PORT && management_host().is_none()
        );
    }

    use super::*;

    // Env-var tests mutate process-global state; keep them in ONE test so the
    // default parallel test runner can't interleave them.
    #[test]
    fn overrides_and_defaults() {
        // SAFETY: `set_var` races any concurrent `var()` in the process, and
        // this is no longer the only place that reads these — `port.rs` reaches
        // `daemon_port()` through `infrastructure_ports()` on every `allocate`
        // and `reserve_fixed`. The exclusion is therefore the crate-wide
        // process-state lock, which the port tests take too; holding it is what
        // makes the mutations below sound. A grep-based "nobody else reads it"
        // argument used to stand here and stopped being true.
        let _guard = crate::test_support::process_state_guard();
        unsafe {
            std::env::remove_var("VELD_DAEMON_PORT");
            assert_eq!(daemon_port(), DEFAULT_DAEMON_PORT);

            std::env::set_var("VELD_DAEMON_PORT", "19898");
            assert_eq!(daemon_port(), 19898);
            assert_eq!(daemon_base(), "http://127.0.0.1:19898");
            assert_eq!(daemon_upstream(), "localhost:19898");

            std::env::set_var("VELD_DAEMON_PORT", "not-a-port");
            assert_eq!(daemon_port(), DEFAULT_DAEMON_PORT);
            std::env::set_var("VELD_DAEMON_PORT", "");
            assert_eq!(daemon_port(), DEFAULT_DAEMON_PORT);
            std::env::remove_var("VELD_DAEMON_PORT");

            std::env::set_var("VELD_DAEMON_SOCK", "/tmp/dev.sock");
            assert_eq!(daemon_socket(), PathBuf::from("/tmp/dev.sock"));
            std::env::remove_var("VELD_DAEMON_SOCK");
            assert!(daemon_socket().ends_with(".veld/daemon.sock"));

            std::env::remove_var("VELD_MANAGEMENT_HOST");
            assert_eq!(management_host(), None);
            std::env::set_var("VELD_MANAGEMENT_HOST", "veld-dev.localhost");
            assert_eq!(management_host().as_deref(), Some("veld-dev.localhost"));
            std::env::remove_var("VELD_MANAGEMENT_HOST");

            // The holder directory does NOT follow the daemon socket, and that
            // is the length bound rather than a preference — see `pty_dir`. A
            // dev socket inside a deep checkout used to drag the holder sockets
            // in with it and overrun `sun_path`.
            std::env::remove_var("VELD_PTY_DIR");
            std::env::set_var(
                "VELD_DAEMON_SOCK",
                "/Users/someone/git/_worktrees/a-long-branch-name/.veld-dev/daemon.sock",
            );
            let dir = pty_dir();
            assert!(dir.starts_with(dirs::home_dir().unwrap()), "{dir:?}");
            assert!(!dir.to_string_lossy().contains("_worktrees"), "{dir:?}");
            std::env::remove_var("VELD_DAEMON_SOCK");

            // …and the override still wins outright, which is how the recovery
            // test points a child at a temp dir.
            std::env::set_var("VELD_PTY_DIR", "/tmp/veld-pty-test");
            assert_eq!(pty_dir(), PathBuf::from("/tmp/veld-pty-test"));
            // The shims follow it, and that is the point of nesting them here: a
            // daemon spawned by a test redirects VELD_PTY_DIR and nothing else, so
            // a shim directory keyed off the port instead wrote into the real
            // `~/.veld` on every `cargo test` run.
            assert_eq!(shim_dir(), PathBuf::from("/tmp/veld-pty-test/shims"));
            std::env::remove_var("VELD_PTY_DIR");
            assert!(shim_dir().starts_with(pty_dir()));

            // The real default has to fit with room for the digest filename, or
            // the bound below is theatre. Asserted here rather than in its own
            // test because `pty_dir` reads the vars this test owns.
            let realistic = pty_dir().join("0123456789abcdef.sock");
            assert_eq!(socket_path_too_long(&realistic), None, "{realistic:?}");

            // --- holder_sockets_in ---------------------------------------
            // The chokepoint every holder-directory scan goes through, because
            // each of them then *acts* on what it found: the daemon deletes a
            // socket nobody answers, `veld uninstall` writes a HANGUP to it, and
            // `veld doctor` connects to it. Pointed at `~/.veld` — which
            // `VELD_PTY_DIR` can be — a `.sock`-extension filter hands all three
            // the daemon's own control socket.
            let dir = tempfile::tempdir().unwrap();
            for name in [
                "0123456789abcdef.sock",
                "fedcba9876543210.sock",
                "daemon.sock",
                "helper.sock",
                "0123456789ABCDEF.sock",
                "0123456789abcdef",
                "notes.txt",
            ] {
                std::fs::write(dir.path().join(name), b"").unwrap();
            }
            std::fs::create_dir(dir.path().join("shims")).unwrap();
            let mut found: Vec<String> = holder_sockets_in(dir.path())
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect();
            found.sort();
            assert_eq!(found, ["0123456789abcdef.sock", "fedcba9876543210.sock"]);
            // A directory that does not exist is the ordinary state — it appears
            // with the first terminal — and must not be an error the caller has to
            // reason about.
            assert!(holder_sockets_in(&dir.path().join("nope")).is_empty());

            // --- dev_trusted_origins -------------------------------------
            // Same test body for the same reason: it reads VELD_DAEMON_PORT.
            std::env::set_var("VELD_URL", "https://dev-daemon.run.veld.localhost");
            std::env::set_var(
                "VELD_PROXY_ORIGINS",
                "http://localhost:5199, http://127.0.0.1:5199 ,,https://*.evil",
            );

            // The INSTALLED daemon gets nothing, whatever the environment says.
            // This is the gate, and it lives inside the function so a caller
            // cannot forget it.
            std::env::remove_var("VELD_DAEMON_PORT");
            assert_eq!(daemon_port(), DEFAULT_DAEMON_PORT);
            assert!(dev_trusted_origins().is_empty(), "installed instance");

            // A dev instance gets its own URL plus the proxying dev servers —
            // and the wildcard entry is dropped rather than widening the gate.
            std::env::set_var("VELD_DAEMON_PORT", "19898");
            assert_eq!(
                dev_trusted_origins(),
                vec![
                    "http://127.0.0.1:5199".to_owned(),
                    "http://localhost:5199".to_owned(),
                    "https://dev-daemon.run.veld.localhost".to_owned(),
                ]
            );

            // A dev instance that is not a veld node has no VELD_URL, and that
            // is not an error — `just dev-daemon` is exactly this case.
            std::env::remove_var("VELD_URL");
            std::env::remove_var("VELD_PROXY_ORIGINS");
            assert!(dev_trusted_origins().is_empty(), "dev instance, no node");

            std::env::remove_var("VELD_DAEMON_PORT");
        }
    }

    /// Pure, so it can be exhaustive without touching process-global env — the
    /// env-reading half is covered inside `overrides_and_defaults`.
    #[test]
    fn only_a_bare_origin_normalizes() {
        for good in [
            "https://dev-daemon.run.veld.localhost",
            "https://dev-daemon.run.veld.localhost/", // one trailing slash is fine
            "http://localhost:5199",
            "http://127.0.0.1:19898",
            "http://[::1]:19898",
            "http://[::1]",
            "https://host:19898/",
        ] {
            assert!(normalize_origin(good).is_some(), "rejected {good}");
        }

        // Anything an exact comparison against a browser's `Origin` could never
        // match, or that widens the gate.
        for bad in [
            "",
            "localhost:5199",           // no scheme
            "ftp://host",               // not a browser origin
            "https://*.veld.localhost", // wildcards are how these get bypassed
            "https://host/path",        // a path is not part of an origin
            "https://host//",           // …and two slashes are a path, not a tidy suffix
            "https://host?q=1",
            "https://user@host",
            "https://host:",
            "https://host:notaport",
            "https://host:99999", // not a u16
            // Parses as a port, but is not how a browser spells one — so the
            // entry could never match, and a dead entry with no warning is
            // worse than a rejection with one.
            "http://host:080",
            "http://host:+80",
            "http://host:80",   // a default port is omitted from an Origin
            "https://host:443", // …likewise
            "https:// host",
            "://host",
            "https://",
        ] {
            assert_eq!(normalize_origin(bad), None, "accepted {bad:?}");
        }

        // Browsers serialise lowercase, so we must too or the rule never fires.
        assert_eq!(
            normalize_origin("HTTPS://Dev-Daemon.Run.Veld.Localhost").as_deref(),
            Some("https://dev-daemon.run.veld.localhost")
        );
    }

    #[test]
    fn socket_path_length_is_reported_before_bind() {
        // `bind`'s own error is "path must be shorter than SUN_LEN", which names
        // neither the path nor the way out, and reaches the user as a terminal
        // that will not open.
        assert_eq!(
            socket_path_too_long(std::path::Path::new("/tmp/a.sock")),
            None
        );

        let long = PathBuf::from(format!("/tmp/{}/0123456789abcdef.sock", "x".repeat(100)));
        let msg = socket_path_too_long(&long).expect("over the limit");
        assert!(msg.contains("VELD_PTY_DIR"), "{msg}");
        assert!(msg.contains(&MAX_SOCKET_PATH.to_string()), "{msg}");

        // The exact boundary, since this is an off-by-one waiting to happen:
        // `sun_path` holds MAX_SOCKET_PATH bytes *including* the NUL.
        let at = PathBuf::from("x".repeat(MAX_SOCKET_PATH));
        assert!(socket_path_too_long(&at).is_some());
        let under = PathBuf::from("x".repeat(MAX_SOCKET_PATH - 1));
        assert_eq!(socket_path_too_long(&under), None);
    }
}

use std::collections::HashSet;
use std::net::{SocketAddr, TcpListener};
use std::sync::Mutex;

use thiserror::Error;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const PORT_RANGE_START: u16 = 19000;
pub const PORT_RANGE_END: u16 = 29999;

/// Ports inside the managed range that belong to veld's own infrastructure and
/// must never be handed to a node.
///
/// The range has always contained the daemon's own port
/// ([`crate::instance::DEFAULT_DAEMON_PORT`] = 19899), and nothing excluded it.
/// The hazard is not theoretical: [`is_port_available`] only keeps a node off
/// the port while the daemon is *listening*, so any run started while the
/// installed daemon is down could be handed 19899 — and then the daemon fails
/// to bind on its next start, for reasons nothing connects back to the run.
///
/// The same collision is what makes the dev instance's origin guard sound.
/// `veld-daemon`'s terminal allowlist trusts extra origins only when
/// `daemon_port() != DEFAULT_DAEMON_PORT`, i.e. "I am not the installed
/// daemon". A dev daemon allocated 19899 would silently answer to that guard as
/// though it were the installed one, and its terminals would stop opening with
/// a 403 a browser cannot show the reason for.
///
/// Both this instance's port and the default are excluded: a dev instance's
/// port is equally not a node's to take.
fn infrastructure_ports() -> [u16; 2] {
    [
        crate::instance::DEFAULT_DAEMON_PORT,
        crate::instance::daemon_port(),
    ]
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum PortError {
    #[error("no available ports in range {}-{}", PORT_RANGE_START, PORT_RANGE_END)]
    Exhausted,

    #[error(
        "port {0} is already in use. It was requested explicitly, so veld will not \
         substitute another — a debugger or client pointed at {0} must reach the process \
         that asked for it. Use \"auto\" to let veld allocate a free port instead."
    )]
    AlreadyInUse(u16),

    #[error(
        "port {0} is veld's own daemon port, so a node cannot bind it — the daemon \
         would fail to start next time with nothing pointing back at this run. Use \
         \"auto\", or pick another port."
    )]
    Infrastructure(u16),
}

// ---------------------------------------------------------------------------
// Port allocator
// ---------------------------------------------------------------------------

/// A reserved port with TCP listeners held to prevent other processes from
/// claiming it. Dropping the reservation (or calling [`PortReservation::release`])
/// frees the port so the child process can bind immediately.
pub struct PortReservation {
    pub port: u16,
    /// Held listeners that block other processes from binding this port.
    /// The Vec is non-empty while the reservation is active.
    _guards: Vec<TcpListener>,
}

impl PortReservation {
    /// Release the port reservation by dropping the guard listeners.
    /// Call this immediately before spawning the child process that will
    /// bind the port, to minimise the race window.
    pub fn release(self) -> u16 {
        // `_guards` are dropped here, freeing the port.
        self.port
    }
}

// Manual Debug impl — TcpListener's Debug output is noisy.
impl std::fmt::Debug for PortReservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PortReservation")
            .field("port", &self.port)
            .finish()
    }
}

/// Tracks allocated ports for a single run and finds free ones.
#[derive(Debug)]
pub struct PortAllocator {
    allocated: Mutex<HashSet<u16>>,
}

impl PortAllocator {
    pub fn new() -> Self {
        Self {
            allocated: Mutex::new(HashSet::new()),
        }
    }

    /// Pre-populate with ports that are already in use by a previous/resumed run.
    pub fn with_reserved(reserved: impl IntoIterator<Item = u16>) -> Self {
        Self {
            allocated: Mutex::new(reserved.into_iter().collect()),
        }
    }

    /// Allocate the next available port from the managed range and return a
    /// [`PortReservation`] that holds TCP listeners on the port. Other
    /// processes will see the port as occupied until the reservation is
    /// released. Call [`PortReservation::release`] immediately before
    /// spawning the child process.
    pub fn allocate(&self) -> Result<PortReservation, PortError> {
        let mut allocated = self.allocated.lock().expect("port allocator lock poisoned");
        let infrastructure = infrastructure_ports();
        for port in PORT_RANGE_START..=PORT_RANGE_END {
            if infrastructure.contains(&port) {
                continue;
            }
            if !allocated.contains(&port) && is_port_available(port) {
                // Port is free — now grab reservation listeners to hold it.
                // If the reservation fails (extremely rare race), skip.
                if let Some(guards) = try_reserve_port(port) {
                    allocated.insert(port);
                    return Ok(PortReservation {
                        port,
                        _guards: guards,
                    });
                }
            }
        }
        Err(PortError::Exhausted)
    }

    /// Reserve a specific port the author named explicitly (`"debug": 9229`).
    ///
    /// Unlike [`allocate`](Self::allocate) this cannot pick a different port, so a
    /// port already taken is a hard error naming it — silently substituting
    /// another would attach the debugger to the wrong place. A fixed port is
    /// discouraged for exactly this reason: it is what breaks parallel worktrees.
    pub fn reserve_fixed(&self, port: u16) -> Result<PortReservation, PortError> {
        // Named explicitly, so the answer is a diagnostic rather than a
        // substitution — `AlreadyInUse` would be a lie when the daemon is down,
        // and granting it would break the daemon's next start.
        if infrastructure_ports().contains(&port) {
            return Err(PortError::Infrastructure(port));
        }
        let mut allocated = self.allocated.lock().expect("port allocator lock poisoned");
        if allocated.contains(&port) {
            return Err(PortError::AlreadyInUse(port));
        }
        match try_reserve_port(port) {
            Some(guards) => {
                allocated.insert(port);
                Ok(PortReservation {
                    port,
                    _guards: guards,
                })
            }
            None => Err(PortError::AlreadyInUse(port)),
        }
    }

    /// Release a previously allocated port.
    pub fn release(&self, port: u16) {
        let mut allocated = self.allocated.lock().expect("port allocator lock poisoned");
        allocated.remove(&port);
    }

    /// Return all currently allocated ports.
    pub fn allocated_ports(&self) -> HashSet<u16> {
        self.allocated
            .lock()
            .expect("port allocator lock poisoned")
            .clone()
    }
}

impl Default for PortAllocator {
    fn default() -> Self {
        Self::new()
    }
}

/// Try to bind TCP listeners to reserve `port` from other processes.
/// Returns the held listeners on success, or `None` if any bind fails.
///
/// We bind on IPv4 wildcard and IPv6 loopback:
/// - `0.0.0.0` covers all IPv4 addresses (including `127.0.0.1`)
/// - `[::1]` covers IPv6 loopback
///
/// We intentionally do NOT also bind `127.0.0.1` because on Linux,
/// binding a specific address after the wildcard on the same port fails
/// with EADDRINUSE (the wildcard already covers it). On macOS this overlap
/// is allowed, but we avoid it for cross-platform correctness.
fn try_reserve_port(port: u16) -> Option<Vec<TcpListener>> {
    let wildcard: SocketAddr = ([0, 0, 0, 0], port).into();
    let ipv6: SocketAddr = ([0, 0, 0, 0, 0, 0, 0, 1], port).into();

    let l1 = TcpListener::bind(wildcard).ok()?;
    let l2 = TcpListener::bind(ipv6).ok()?;
    Some(vec![l1, l2])
}

/// Check whether a TCP port is available by attempting to bind on all
/// relevant address families: IPv4 loopback, IPv6 loopback, and IPv4
/// wildcard (`0.0.0.0`).
///
/// Modern runtimes (Node.js 18+, Next.js, etc.) often default to IPv6.
/// Docker containers bind on `0.0.0.0`. A stale process on any of these
/// addresses would cause the new process to fail, so we check all three.
///
/// Each bind is attempted and immediately dropped, so there is no overlap
/// issue between addresses (unlike `try_reserve_port` which holds them).
pub fn is_port_available(port: u16) -> bool {
    let ipv4: SocketAddr = ([127, 0, 0, 1], port).into();
    let ipv6: SocketAddr = ([0, 0, 0, 0, 0, 0, 0, 1], port).into();
    let wildcard: SocketAddr = ([0, 0, 0, 0], port).into();

    // Each must succeed independently — drop before the next to avoid
    // same-process overlap on Linux.
    if TcpListener::bind(ipv4).is_err() {
        return false;
    }
    if TcpListener::bind(ipv6).is_err() {
        return false;
    }
    if TcpListener::bind(wildcard).is_err() {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Serializes the tests that bind real ports.
    ///
    /// `cargo test` runs a crate's tests in one process on many threads, and every
    /// `PortAllocator::new()` scans the same range from the same start. Two tests
    /// running concurrently therefore race for the same port number — most visibly
    /// in the window between `reservation.release()` and re-binding to prove the
    /// port is free, where a sibling test can legitimately take it first. That is a
    /// test-harness artefact, not a bug in the allocator, so the tests take a lock
    /// rather than the allocator growing a global.
    /// The crate-wide process-state lock, not a port-only one.
    ///
    /// `allocate` and `reserve_fixed` reach `instance::daemon_port()`, which
    /// reads `VELD_DAEMON_PORT` — so these tests race `instance`'s env test
    /// unless both take the *same* mutex. See `crate::test_support`.
    use crate::test_support::process_state_guard as port_guard;

    #[test]
    fn test_available_port_is_detected() {
        let _guard = port_guard();
        // Port 0 lets the OS pick a free port; use it to find one that's free.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        // After dropping, the port should be available.
        assert!(is_port_available(port));
    }

    #[test]
    fn test_occupied_port_is_detected() {
        let _guard = port_guard();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // Port is still held — should NOT be available.
        assert!(!is_port_available(port));
        drop(listener);
    }

    #[test]
    fn test_wildcard_occupied_port_is_detected() {
        let _guard = port_guard();
        let listener = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], 0))).unwrap();
        let port = listener.local_addr().unwrap().port();
        // Wildcard binding occupies the port on all interfaces.
        assert!(!is_port_available(port));
        drop(listener);
    }

    #[test]
    fn test_allocator_skips_occupied_ports() {
        let _guard = port_guard();
        // Find the first port the allocator would pick, then occupy it.
        let allocator = PortAllocator::new();
        let first_reservation = allocator.allocate().unwrap();
        let first_port = first_reservation.port;
        allocator.release(first_port);
        // Release the reservation so we can manually occupy the port.
        first_reservation.release();

        // Now occupy that port and allocate again — should skip it.
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], first_port))).unwrap();
        let second_reservation = allocator.allocate().unwrap();
        assert_ne!(second_reservation.port, first_port);
        drop(listener);
    }

    /// F6: an explicitly requested port is never substituted.
    ///
    /// `allocate` may pick any free port; `reserve_fixed` may not. A debugger or
    /// client pointed at 9229 must reach the process that asked for 9229, so a
    /// taken port is an error naming it rather than a silent move to 9230.
    #[test]
    fn test_reserve_fixed_refuses_to_substitute() {
        let _guard = port_guard();

        // Find a port that is genuinely free, then forget it: `allocate` records
        // the port in the allocator's own set, and asking the *same* allocator for
        // it again is legitimately "already in use".
        let free_port = {
            let scout = PortAllocator::new();
            let reservation = scout.allocate().expect("a port is free");
            reservation.release()
        };

        let allocator = PortAllocator::new();
        let reservation = allocator
            .reserve_fixed(free_port)
            .expect("a free fixed port is reserved");
        assert_eq!(reservation.port, free_port, "never a different port");
        let taken = free_port;

        // Asking again while it is held fails, naming the port.
        let err = allocator.reserve_fixed(taken).unwrap_err();
        assert!(matches!(err, PortError::AlreadyInUse(p) if p == taken));
        assert!(
            err.to_string().contains(&taken.to_string()),
            "the message must name the port: {err}"
        );
        // …and suggests the alternative rather than just refusing.
        assert!(err.to_string().contains("auto"), "{err}");

        reservation.release();
    }

    #[test]
    fn test_reservation_holds_port() {
        let _guard = port_guard();
        let allocator = PortAllocator::new();
        let reservation = allocator.allocate().unwrap();
        let port = reservation.port;

        // While the reservation is held, binding the same wildcard address should fail.
        let wildcard: SocketAddr = ([0, 0, 0, 0], port).into();
        let bind_result = TcpListener::bind(wildcard);
        assert!(
            bind_result.is_err(),
            "port {port} should be held by reservation"
        );

        // After releasing, binding should succeed.
        reservation.release();
        let bind_result = TcpListener::bind(wildcard);
        assert!(
            bind_result.is_ok(),
            "port {port} should be free after release"
        );
    }

    /// The daemon's port is inside the managed range, and `is_port_available`
    /// only keeps a node off it while the daemon is listening — so the exclusion
    /// has to be structural, not a side effect of the daemon being up.
    #[test]
    fn the_daemon_port_is_never_handed_to_a_node() {
        assert!(
            (PORT_RANGE_START..=PORT_RANGE_END).contains(&crate::instance::DEFAULT_DAEMON_PORT),
            "if the daemon port ever moves out of the range, this exclusion is dead \
             code and should go — but until then it is load-bearing"
        );

        let allocator = PortAllocator::new();
        let err = allocator
            .reserve_fixed(crate::instance::DEFAULT_DAEMON_PORT)
            .unwrap_err();
        assert!(
            matches!(err, PortError::Infrastructure(p) if p == crate::instance::DEFAULT_DAEMON_PORT),
            "{err}"
        );
        // Refusing is only useful if the message says which port and why.
        assert!(
            err.to_string()
                .contains(&crate::instance::DEFAULT_DAEMON_PORT.to_string()),
            "{err}"
        );
        assert!(err.to_string().contains("daemon"), "{err}");
    }

    /// `allocate` skips it rather than erroring — a node asking for "auto" has
    /// expressed no opinion, so the right answer is the next port.
    #[test]
    fn allocate_skips_the_daemon_port() {
        let _guard = port_guard();
        assert!(
            !infrastructure_ports().is_empty(),
            "the skip in `allocate` is keyed on this list"
        );
        let excluded = infrastructure_ports();

        // Every port below the daemon's is claimed, so an unfiltered ascending
        // scan would return the daemon's port next. Reserving them in the
        // allocator's own set (rather than binding 899 sockets) is what makes
        // this a unit test.
        let below: Vec<u16> = (PORT_RANGE_START..crate::instance::DEFAULT_DAEMON_PORT).collect();
        let allocator = PortAllocator::with_reserved(below);
        let reservation = allocator.allocate().expect("a port above the daemon's");
        assert!(
            !excluded.contains(&reservation.port),
            "allocated {}, which is veld's own",
            reservation.port
        );
        reservation.release();
    }
}

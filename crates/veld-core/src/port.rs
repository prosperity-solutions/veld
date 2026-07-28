use std::collections::HashSet;
use std::net::{SocketAddr, TcpListener};
use std::sync::Mutex;

use thiserror::Error;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const PORT_RANGE_START: u16 = 19000;
pub const PORT_RANGE_END: u16 = 29999;

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
        for port in PORT_RANGE_START..=PORT_RANGE_END {
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
    static PORT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Take the port lock, recovering from a poisoned mutex so one failing test
    /// does not cascade into every other port test failing too.
    fn port_guard() -> std::sync::MutexGuard<'static, ()> {
        PORT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

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
}

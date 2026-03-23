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

    #[test]
    fn test_available_port_is_detected() {
        // Port 0 lets the OS pick a free port; use it to find one that's free.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        // After dropping, the port should be available.
        assert!(is_port_available(port));
    }

    #[test]
    fn test_occupied_port_is_detected() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // Port is still held — should NOT be available.
        assert!(!is_port_available(port));
        drop(listener);
    }

    #[test]
    fn test_wildcard_occupied_port_is_detected() {
        let listener = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], 0))).unwrap();
        let port = listener.local_addr().unwrap().port();
        // Wildcard binding occupies the port on all interfaces.
        assert!(!is_port_available(port));
        drop(listener);
    }

    #[test]
    fn test_allocator_skips_occupied_ports() {
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

    #[test]
    fn test_reservation_holds_port() {
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

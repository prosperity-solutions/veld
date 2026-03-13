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

    /// Allocate the next available port from the managed range.
    pub fn allocate(&self) -> Result<u16, PortError> {
        let mut allocated = self.allocated.lock().expect("port allocator lock poisoned");
        for port in PORT_RANGE_START..=PORT_RANGE_END {
            if !allocated.contains(&port) && is_port_available(port) {
                allocated.insert(port);
                return Ok(port);
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

/// Check whether a TCP port is available by attempting to bind on all
/// relevant address families: IPv4 loopback, IPv6 loopback, and IPv4
/// wildcard (`0.0.0.0`).
///
/// Modern runtimes (Node.js 18+, Next.js, etc.) often default to IPv6.
/// Docker containers bind on `0.0.0.0`. A stale process on any of these
/// addresses would cause the new process to fail, so we check all three.
pub fn is_port_available(port: u16) -> bool {
    let ipv4: SocketAddr = ([127, 0, 0, 1], port).into();
    let ipv6: SocketAddr = ([0, 0, 0, 0, 0, 0, 0, 1], port).into();
    let wildcard: SocketAddr = ([0, 0, 0, 0], port).into();

    // All must succeed — if any is in use, the port is occupied.
    TcpListener::bind(ipv4).is_ok()
        && TcpListener::bind(ipv6).is_ok()
        && TcpListener::bind(wildcard).is_ok()
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
        let first_port = allocator.allocate().unwrap();
        allocator.release(first_port);

        // Now occupy that port and allocate again — should skip it.
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], first_port))).unwrap();
        let second_port = allocator.allocate().unwrap();
        assert_ne!(second_port, first_port);
        drop(listener);
    }
}

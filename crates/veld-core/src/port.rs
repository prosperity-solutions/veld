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

/// Check whether a TCP port is available by attempting to bind on both
/// IPv4 (127.0.0.1) and IPv6 (::1).
///
/// Modern runtimes (Node.js 18+, Next.js, etc.) often default to IPv6.
/// A stale process on `[::1]:port` would pass an IPv4-only check, so we
/// must verify both address families.
pub fn is_port_available(port: u16) -> bool {
    let ipv4: SocketAddr = ([127, 0, 0, 1], port).into();
    let ipv6: SocketAddr = ([0, 0, 0, 0, 0, 0, 0, 1], port).into();

    // Both must succeed — if either is in use, the port is occupied.
    TcpListener::bind(ipv4).is_ok() && TcpListener::bind(ipv6).is_ok()
}

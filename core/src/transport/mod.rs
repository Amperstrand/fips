//! Transport value types.
//!
//! The identifiers, address and statistics types a transport and the
//! protocol cores both name. The transports themselves, which own sockets
//! and devices, stay in the root crate, which re-exports these types under
//! `fips::transport`.

mod types;

pub use types::{LinkDirection, LinkId, LinkStats, TransportAddr, TransportId};

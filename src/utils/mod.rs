//! Utility modules.
//!
//! Shared infrastructure that doesn't belong to a specific protocol layer:
//! session index allocation, socket permission handling, and other
//! cross-cutting concerns.

pub mod index;
#[cfg(unix)]
pub mod sockperm;

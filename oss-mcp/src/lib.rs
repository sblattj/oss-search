//! Library surface for `oss-mcp` binary internals.
//!
//! Exists so integration tests (and future library consumers) can reach
//! items such as [`version::VERSION`] that the binary target alone could
//! not expose.

pub mod version;

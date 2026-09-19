//! Schema constants for the library file format.
//!
//! Schema migration is intentionally not implemented; the library file is
//! authored by hand and we reject anything that doesn't match the current
//! version exactly.

/// The only schema version this build accepts.
pub const SCHEMA_VERSION: u32 = 1;

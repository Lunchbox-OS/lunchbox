//! Client-mirror generation for the shepherdd wire protocol.
//!
//! Lives in its own leaf crate for two reasons. It sits above `shepherd-ble`
//! in the dependency graph, so it can describe the BLE claim types alongside
//! the management payloads — a generator inside `shepherd-management` could
//! not, since `shepherd-ble` depends on it. And being outside the workspace's
//! `default-members` keeps `schemars` out of the binaries that ship.

pub mod kotlin_types;
pub mod wire_schema;

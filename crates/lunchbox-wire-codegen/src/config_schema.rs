//! JSON Schema for the `config.toml` types, the source of truth for the web
//! config editor's TypeScript mirrors.
//!
//! The editor renders a projection of `RawConfig` — what `serde` produces when
//! the daemon's own parser reads a file — so these types describe the
//! *deserialized* shape, not TOML syntax. Where the file format accepts a
//! shorthand (`input_compat = "touch_to_mouse"` as well as a list), the
//! projection always carries the canonical form, which is what the generated
//! types state.

use schemars::{Schema, schema_for};

/// Schema for the whole config document, with every nested type in `$defs`.
pub fn config_schema() -> Schema {
    schema_for!(lunchbox_config::RawConfig)
}

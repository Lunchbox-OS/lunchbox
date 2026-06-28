//! Platform-agnostic application layer for `shepherd-media`.
//!
//! See the crate README for an overview and
//! `docs/ai/history/2026-06-27 001 shepherd-media android architecture.md` for
//! the design rationale.
//!
//! The Linux binary is stateless (driven by `shepherdd` via CLI flags); the
//! Android binary, with no `shepherdd` and a single install per device, needs
//! persistent state for its configured libraries and per-library caching
//! options. That state and the operations on it live here.

pub mod quality;
pub mod settings;

pub use quality::{CacheMode, PosterPolicy, Quality};
pub use settings::{
    AppSettings, CachingSettings, LibraryEntry, LibrarySource, SETTINGS_SCHEMA_VERSION,
    SettingsError, SettingsIoError,
};

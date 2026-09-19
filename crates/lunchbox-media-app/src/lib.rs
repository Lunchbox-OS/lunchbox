//! Platform-agnostic application layer for `lunchbox-media`.
//!
//! See the crate README for an overview and
//! `docs/ai/history/2026-06-27 001 lunchbox-media android architecture.md` for
//! the design rationale.
//!
//! The Linux binary is near-stateless (driven by `lunchboxd` via CLI flags); the
//! Android binary, with no `lunchboxd` and a single install per device, needs
//! persistent state for its configured libraries and per-library caching
//! options. That state and the operations on it live here.
//!
//! The exception is [`resume`], the opt-in playback-position store, which *both*
//! front-ends use — it lives here so the two share one policy for what counts as
//! finished, what is too early to remember, and how often to write.

pub mod cache;
pub mod cache_key;
pub mod interest;
pub mod lru;
pub mod poster_cache;
pub mod quality;
pub mod resume;
pub mod settings;
pub mod sponsorblock;

pub use cache::{Freshness, Resolution};
pub use cache_key::{SPONSORBLOCK_PREFIX_LEN, content_key, interest_key, sponsorblock_prefix};
pub use lru::{Score, ScoreWeights, Standing};
pub use poster_cache::RemotePosterCache;
pub use quality::{CacheMode, PosterPolicy, Quality};
pub use resume::{ItemPosition, RESUME_SCHEMA_VERSION, ResumeState, ResumeStore, ResumeTracker};
pub use settings::{
    AppSettings, CachingSettings, LibraryEntry, LibrarySource, SETTINGS_SCHEMA_VERSION,
    SettingsError, SettingsIoError,
};
pub use sponsorblock::BucketStore;

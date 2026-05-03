//! Platform-agnostic core for `shepherd-media`.
//!
//! See the crate-level README and `docs/shepherd-media.md` for an overview;
//! see the spec at `docs/ai/history/2026-05-02 007 media launcher.md` for the
//! design rationale.

pub mod library;
pub mod player;
pub mod protocol;
pub mod resolver;
pub mod schema;
pub mod session;
pub mod uri;

pub use library::{
    ClassifiedUri, Item, ItemKind, Library, LibraryError, Platform, PlayerHint, PosterRef, Source,
    load_library,
};
pub use player::{PlayerError, PlayerEvent, PlayerHandle};
pub use protocol::{ProtocolEmitter, ProtocolEvent, UriClass};
pub use resolver::{PlatformInfo, resolve_source};
pub use session::{Session, SessionInput, SessionState};
pub use uri::DrmRejection;

#[cfg(feature = "libmpv")]
pub use player::LibmpvPlayer;

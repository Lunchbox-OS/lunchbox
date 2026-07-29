//! Platform-agnostic core for `shepherd-media`.
//!
//! See the crate-level README and `docs/shepherd-media.md` for an overview;
//! see the spec at `docs/ai/history/2026-05-02 007 media launcher.md` for the
//! design rationale.

pub mod library;
pub mod player;
pub mod playlist;
pub mod protocol;
pub mod resolver;
pub mod schema;
pub mod session;
pub mod uri;
pub mod youtube;
pub mod youtube_playlist;

pub use library::{
    ClassifiedUri, Item, ItemKind, Library, LibraryError, Platform, PlayerHint, PosterRef, Source,
    load_library,
};
pub use player::{
    GetProcAddress, NativeDisplay, PlayerError, PlayerEvent, PlayerHandle, RetryBudget, Transport,
    VideoOutput,
};
pub use protocol::{ProtocolEmitter, ProtocolEvent, UriClass};
pub use resolver::{PlatformInfo, resolve_source};
pub use session::{Session, SessionInput, SessionState};
pub use uri::DrmRejection;
pub use youtube::YOUTUBE_EXTRACTOR_ARGS;
pub use youtube_playlist::{
    PlaylistInfo, YoutubePlaylistEntry, build_library_from_entries, is_youtube_playlist_url,
    parse_flat_playlist,
};

#[cfg(feature = "libmpv")]
pub use player::LibmpvPlayer;

//! Linux host adapter for shepherdd
//!
//! Provides:
//! - Process spawning with process group isolation
//! - Graceful (SIGTERM) and forceful (SIGKILL) termination
//! - Exit observation
//! - stdout/stderr capture
//! - Volume control with auto-detection of sound systems
//! - Screen brightness control via sysfs / `brightnessctl`
//! - Ambient-light-sensor reads via IIO sysfs (for automatic brightness)

mod adapter;
mod audio;
mod audio_route;
mod brightness;
mod browser;
mod ebook;
pub mod helpers;
mod light;
mod process;
mod retroarch;
mod sidecar;
mod steam_interstitial;
mod sway;
pub mod sway_ipc;
mod volume;

pub use adapter::*;
pub use audio::{AudioOutput, AudioOutputKind, AudioTopology, SinkNode};
pub use audio_route::{AudioRouter, NoOpAudioRouter, PipeWireAudioRouter};
pub use brightness::*;
pub use browser::is_supported_browser_flatpak;
pub use ebook::{MissingSupport, missing_book, missing_support};
pub use helpers::{
    resolve as resolve_helper, resolve_arg as resolve_helper_arg, resolve_daemon_sibling,
};
pub use light::*;
pub use process::*;
pub use retroarch::{MissingCore, missing_content, missing_core};
pub use sway::{
    DisplayInfo, OutputBackend, OutputScale, SwayIpcBackend, disable_output, enable_output,
    get_displays, get_outputs, map_pointer_to_output, move_to_output_fullscreen, pick_mirror_mode,
    select_primary, set_output_mode, set_output_scale,
};
pub use volume::*;

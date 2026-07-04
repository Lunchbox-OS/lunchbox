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
mod brightness;
mod browser;
mod light;
mod process;
mod sidecar;
mod steam_interstitial;
mod sway;
mod volume;

pub use adapter::*;
pub use brightness::*;
pub use light::*;
pub use process::*;
pub use sway::{OutputScale, get_outputs, set_output_scale};
pub use volume::*;

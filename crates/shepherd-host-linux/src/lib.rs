//! Linux host adapter for shepherdd
//!
//! Provides:
//! - Process spawning with process group isolation
//! - Graceful (SIGTERM) and forceful (SIGKILL) termination
//! - Exit observation
//! - stdout/stderr capture
//! - Volume control with auto-detection of sound systems
//! - Screen brightness control via sysfs / `brightnessctl`

mod adapter;
mod brightness;
mod process;
mod sidecar;
mod sway;
mod volume;

pub use adapter::*;
pub use brightness::*;
pub use process::*;
pub use sway::{OutputScale, get_outputs, set_output_scale};
pub use volume::*;

//! Linux platform helpers exposed to the binary.
//!
//! The core handles platform detection internally for source resolution; this
//! module exists so the binary has a single place to pin what the Linux build
//! reports without leaking the choice into other modules.

use shepherd_media_core::{Platform, PlatformInfo};

pub fn current() -> PlatformInfo {
    PlatformInfo {
        platform: Platform::Linux,
    }
}

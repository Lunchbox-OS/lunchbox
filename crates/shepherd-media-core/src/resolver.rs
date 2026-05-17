//! Source selection for a given runtime platform.

use crate::library::{Item, Platform, Source};

#[derive(Debug, Clone, Copy)]
pub struct PlatformInfo {
    pub platform: Platform,
}

impl PlatformInfo {
    /// Build a `PlatformInfo` for the platform this binary is currently
    /// compiled against. The Linux binary always reports `Platform::Linux`;
    /// the (future) Android binary will report `Platform::Android`.
    pub fn current() -> Self {
        Self {
            platform: current_platform(),
        }
    }
}

/// First-match-wins source resolution: returns the first source whose
/// `platforms` list either includes the running platform or `Any`.
pub fn resolve_source<'a>(item: &'a Item, info: &PlatformInfo) -> Option<&'a Source> {
    item.sources
        .iter()
        .find(|s| s.platforms.iter().any(|p| matches(*p, info.platform)))
}

fn matches(declared: Platform, current: Platform) -> bool {
    declared == Platform::Any || declared == current
}

#[cfg(target_os = "android")]
fn current_platform() -> Platform {
    Platform::Android
}

#[cfg(not(target_os = "android"))]
fn current_platform() -> Platform {
    Platform::Linux
}

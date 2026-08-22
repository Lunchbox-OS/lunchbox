//! Volume control trait interfaces
//!
//! Defines the capability-based interface for volume control between
//! the shepherdd service and platform-specific implementations.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors from volume control operations
#[derive(Debug, Error)]
pub enum VolumeError {
    #[error("Volume control not available: {0}")]
    NotAvailable(String),

    #[error("Backend error: {0}")]
    Backend(String),

    #[error("Volume out of range: {0}")]
    OutOfRange(u8),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type VolumeResult<T> = Result<T, VolumeError>;

/// Volume status
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct VolumeStatus {
    /// Volume percentage (0-100, can exceed if system allows)
    pub percent: u8,
    /// Whether audio is muted
    pub muted: bool,
}

impl VolumeStatus {
    /// Get an icon name for the current volume status
    pub fn icon_name(&self) -> &'static str {
        if self.muted || self.percent == 0 {
            "audio-volume-muted-symbolic"
        } else if self.percent < 33 {
            "audio-volume-low-symbolic"
        } else if self.percent < 66 {
            "audio-volume-medium-symbolic"
        } else {
            "audio-volume-high-symbolic"
        }
    }
}

/// Volume capabilities
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VolumeCapabilities {
    /// Whether volume control is available
    pub available: bool,
    /// The detected sound backend (e.g., "pipewire", "pulseaudio", "alsa")
    pub backend: Option<String>,
    /// Whether mute control is available
    pub can_mute: bool,
    /// Maximum volume percentage allowed (for systems that allow >100%)
    pub max_volume: u8,
}

/// Volume restrictions that can be enforced by policy
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VolumeRestrictions {
    /// Maximum volume percentage allowed (enforced by the service)
    pub max_volume: Option<u8>,
    /// Minimum volume percentage allowed (enforced by the service)
    pub min_volume: Option<u8>,
    /// Whether mute toggle is allowed
    pub allow_mute: bool,
    /// Whether volume changes are allowed at all
    pub allow_change: bool,
}

impl VolumeRestrictions {
    /// Create unrestricted volume settings
    pub fn unrestricted() -> Self {
        Self {
            max_volume: None,
            min_volume: None,
            allow_mute: true,
            allow_change: true,
        }
    }

    /// Clamp a volume value to the allowed range
    pub fn clamp_volume(&self, percent: u8) -> u8 {
        let min = self.min_volume.unwrap_or(0);
        let max = self.max_volume.unwrap_or(100);
        percent.clamp(min, max)
    }
}

/// Everything one read of the sound system says.
///
/// The reading, the output it applies to, and every output that could be
/// switched to all come from the same read, so they can never describe two
/// different moments. That mattered before this type existed: reading the status
/// and the identity separately published a torn pair on exactly the sink switch
/// the watcher exists to report.
#[derive(Debug, Clone, Default)]
pub struct AudioSnapshot {
    pub status: VolumeStatus,
    /// The output currently selected. `None` on hosts that cannot enumerate
    /// outputs, or when no default sink resolves.
    pub active: Option<shepherd_api::AudioOutput>,
    /// Every output present and usable right now, including `active`. Empty on
    /// hosts that cannot enumerate — not a claim that there is no sound, only
    /// that there is nothing to choose between.
    pub outputs: Vec<shepherd_api::AudioOutput>,
}

impl AudioSnapshot {
    /// Identity key of the selected output.
    pub fn active_key(&self) -> Option<&str> {
        self.active.as_ref().map(|o| o.key.as_str())
    }
}

/// Volume controller trait - implemented by platform-specific adapters
#[async_trait]
pub trait VolumeController: Send + Sync {
    /// Get the capabilities of this volume controller
    fn capabilities(&self) -> &VolumeCapabilities;

    /// Get current volume status
    async fn get_status(&self) -> VolumeResult<VolumeStatus>;

    /// Set volume to a specific percentage
    async fn set_volume(&self, percent: u8) -> VolumeResult<()>;

    /// Increase volume by a step
    async fn volume_up(&self, step: u8) -> VolumeResult<()>;

    /// Decrease volume by a step
    async fn volume_down(&self, step: u8) -> VolumeResult<()>;

    /// Toggle mute state
    async fn toggle_mute(&self) -> VolumeResult<()>;

    /// Set mute state explicitly
    async fn set_mute(&self, muted: bool) -> VolumeResult<()>;

    /// The audio output the current reading applies to.
    ///
    /// Defaults to `None`: hosts with no way to enumerate outputs (PulseAudio,
    /// ALSA, no sound backend at all) keep their existing single-output
    /// behaviour rather than being forced to invent an identity.
    async fn current_output(&self) -> Option<shepherd_api::AudioOutput> {
        None
    }

    /// Make `output_key` the output sound plays out of.
    ///
    /// Errors by default rather than silently doing nothing: a caller that asked
    /// to move the audio and got `Ok` back would have no way to learn that it
    /// never moved.
    async fn select_output(&self, output_key: &str) -> VolumeResult<()> {
        let _ = output_key;
        Err(VolumeError::NotAvailable(
            "this sound backend has no selectable outputs".into(),
        ))
    }

    /// Read the whole audio picture as one consistent snapshot.
    ///
    /// This is the only read the daemon should use when it cares about more than
    /// the bare percentage: everything in an [`AudioSnapshot`] comes from one
    /// look at the system, so no two fields can describe different moments.
    ///
    /// The default composes two independent reads and reports only the active
    /// output, which is all a backend that cannot enumerate has to offer.
    /// Backends that can read everything at once should override it — the watch
    /// loop leans on the single read both for consistency and so that noticing a
    /// device being plugged in costs nothing beyond the poll it already does.
    async fn observe(&self) -> VolumeResult<AudioSnapshot> {
        let status = self.get_status().await?;
        let active = self.current_output().await;
        Ok(AudioSnapshot {
            status,
            outputs: active.iter().cloned().collect(),
            active,
        })
    }
}

/// No-op [`VolumeController`] for tests and hosts with no sound backend.
/// Reports unavailable and accepts (ignores) every change.
#[derive(Default)]
pub struct NoOpVolumeController {
    capabilities: VolumeCapabilities,
}

#[async_trait]
impl VolumeController for NoOpVolumeController {
    fn capabilities(&self) -> &VolumeCapabilities {
        &self.capabilities
    }
    async fn get_status(&self) -> VolumeResult<VolumeStatus> {
        Ok(VolumeStatus::default())
    }
    async fn set_volume(&self, _percent: u8) -> VolumeResult<()> {
        Ok(())
    }
    async fn volume_up(&self, _step: u8) -> VolumeResult<()> {
        Ok(())
    }
    async fn volume_down(&self, _step: u8) -> VolumeResult<()> {
        Ok(())
    }
    async fn toggle_mute(&self) -> VolumeResult<()> {
        Ok(())
    }
    async fn set_mute(&self, _muted: bool) -> VolumeResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_volume_icon_names() {
        let status = VolumeStatus {
            percent: 0,
            muted: false,
        };
        assert_eq!(status.icon_name(), "audio-volume-muted-symbolic");

        let status = VolumeStatus {
            percent: 50,
            muted: false,
        };
        assert_eq!(status.icon_name(), "audio-volume-medium-symbolic");

        let status = VolumeStatus {
            percent: 100,
            muted: true,
        };
        assert_eq!(status.icon_name(), "audio-volume-muted-symbolic");
    }

    #[test]
    fn test_restrictions_clamp() {
        let restrictions = VolumeRestrictions {
            max_volume: Some(80),
            min_volume: Some(20),
            allow_mute: true,
            allow_change: true,
        };

        assert_eq!(restrictions.clamp_volume(50), 50);
        assert_eq!(restrictions.clamp_volume(10), 20);
        assert_eq!(restrictions.clamp_volume(90), 80);
    }

    #[test]
    fn test_unrestricted() {
        let restrictions = VolumeRestrictions::unrestricted();
        assert_eq!(restrictions.clamp_volume(0), 0);
        assert_eq!(restrictions.clamp_volume(100), 100);
        assert!(restrictions.allow_mute);
        assert!(restrictions.allow_change);
    }
}

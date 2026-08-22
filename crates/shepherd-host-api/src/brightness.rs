//! Screen brightness control trait interfaces
//!
//! Defines the capability-based interface for screen-brightness control
//! between the shepherdd service and platform-specific implementations.
//! Mirrors [`crate::volume`] so consumers can wire the two indicators the
//! same way.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors from brightness control operations
#[derive(Debug, Error)]
pub enum BrightnessError {
    #[error("Brightness control not available: {0}")]
    NotAvailable(String),

    #[error("Backend error: {0}")]
    Backend(String),

    #[error("Brightness out of range: {0}")]
    OutOfRange(u8),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type BrightnessResult<T> = Result<T, BrightnessError>;

/// Brightness status
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrightnessStatus {
    /// Brightness percentage (0-100)
    pub percent: u8,
}

impl BrightnessStatus {
    /// Get an icon name for the current brightness status
    pub fn icon_name(&self) -> &'static str {
        if self.percent < 33 {
            "display-brightness-low-symbolic"
        } else if self.percent < 66 {
            "display-brightness-medium-symbolic"
        } else {
            "display-brightness-high-symbolic"
        }
    }
}

/// Brightness capabilities
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BrightnessCapabilities {
    /// Whether brightness control is available
    pub available: bool,
    /// The detected backend (e.g., "sysfs", "brightnessctl")
    pub backend: Option<String>,
    /// Name of the backlight device being controlled (sysfs device name)
    pub device: Option<String>,
}

/// Brightness restrictions that can be enforced by policy
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BrightnessRestrictions {
    /// Maximum brightness percentage allowed (enforced by the service)
    pub max_brightness: Option<u8>,
    /// Minimum brightness percentage allowed (enforced by the service)
    pub min_brightness: Option<u8>,
    /// Whether brightness changes are allowed at all
    pub allow_change: bool,
}

impl BrightnessRestrictions {
    /// Create unrestricted brightness settings
    pub fn unrestricted() -> Self {
        Self {
            max_brightness: None,
            min_brightness: None,
            allow_change: true,
        }
    }

    /// Clamp a brightness value to the allowed range
    pub fn clamp_brightness(&self, percent: u8) -> u8 {
        let min = self.min_brightness.unwrap_or(0);
        let max = self.max_brightness.unwrap_or(100);
        percent.clamp(min, max)
    }
}

/// Brightness controller trait - implemented by platform-specific adapters
#[async_trait]
pub trait BrightnessController: Send + Sync {
    /// Get the capabilities of this brightness controller
    fn capabilities(&self) -> &BrightnessCapabilities;

    /// Get current brightness status
    async fn get_status(&self) -> BrightnessResult<BrightnessStatus>;

    /// Set brightness to a specific percentage
    async fn set_brightness(&self, percent: u8) -> BrightnessResult<()>;
}

/// No-op [`BrightnessController`] for tests and hosts with no backlight.
/// Reports unavailable and accepts (ignores) every change.
#[derive(Default)]
pub struct NoOpBrightnessController {
    capabilities: BrightnessCapabilities,
}

#[async_trait]
impl BrightnessController for NoOpBrightnessController {
    fn capabilities(&self) -> &BrightnessCapabilities {
        &self.capabilities
    }
    async fn get_status(&self) -> BrightnessResult<BrightnessStatus> {
        Ok(BrightnessStatus::default())
    }
    async fn set_brightness(&self, _percent: u8) -> BrightnessResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_brightness_icon_names() {
        assert_eq!(
            BrightnessStatus { percent: 0 }.icon_name(),
            "display-brightness-low-symbolic"
        );
        assert_eq!(
            BrightnessStatus { percent: 50 }.icon_name(),
            "display-brightness-medium-symbolic"
        );
        assert_eq!(
            BrightnessStatus { percent: 100 }.icon_name(),
            "display-brightness-high-symbolic"
        );
    }

    #[test]
    fn test_restrictions_clamp() {
        let restrictions = BrightnessRestrictions {
            max_brightness: Some(80),
            min_brightness: Some(20),
            allow_change: true,
        };

        assert_eq!(restrictions.clamp_brightness(50), 50);
        assert_eq!(restrictions.clamp_brightness(10), 20);
        assert_eq!(restrictions.clamp_brightness(90), 80);
    }

    #[test]
    fn test_unrestricted() {
        let restrictions = BrightnessRestrictions::unrestricted();
        assert_eq!(restrictions.clamp_brightness(0), 0);
        assert_eq!(restrictions.clamp_brightness(100), 100);
        assert!(restrictions.allow_change);
    }
}

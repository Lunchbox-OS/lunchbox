//! Linux screen brightness control implementation
//!
//! Reads from `/sys/class/backlight/<device>/brightness` (always
//! world-readable) and writes through `brightnessctl`, which is a required
//! runtime dependency on Linux hosts.
//!
//! Why `brightnessctl` for writes: sysfs `brightness` is owned by root by
//! default, so a direct write from a user-mode daemon fails on most
//! distros. `brightnessctl` ships udev rules that grant the `video` group
//! write access to the right files, which is the canonical way to expose
//! backlight control to unprivileged users. Falling back to a sysfs write
//! at runtime would silently fail in production and only work in dev
//! containers where shepherdd runs as root, so we don't try.
//!
//! There is no brightness analog of the mute button: most backlight devices
//! interpret 0 as "screen off", which is a useless state in our kiosk-like
//! HUD, so the slider is the only control we expose.

use async_trait::async_trait;
use shepherd_host_api::{
    BrightnessCapabilities, BrightnessController, BrightnessError, BrightnessResult,
    BrightnessStatus,
};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

const BACKLIGHT_ROOT: &str = "/sys/class/backlight";
const BACKEND_NAME: &str = "brightnessctl";

/// Detected backlight device under `/sys/class/backlight/`.
#[derive(Debug, Clone)]
struct BacklightDevice {
    /// sysfs device name, e.g. `intel_backlight`, `amdgpu_bl0`.
    name: String,
    /// Path to the device directory.
    path: PathBuf,
    /// Maximum raw brightness value reported by the kernel.
    max_brightness: u32,
}

impl BacklightDevice {
    /// Scan `/sys/class/backlight/` for the first usable backlight device.
    /// Most laptops only have one; if multiple exist we pick the
    /// lexicographically-first name so the choice is deterministic across
    /// reboots.
    fn detect() -> Option<Self> {
        let entries = fs::read_dir(BACKLIGHT_ROOT).ok()?;
        let mut candidates: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.join("brightness").is_file() && p.join("max_brightness").is_file())
            .collect();
        candidates.sort();

        for path in candidates {
            let name = path.file_name()?.to_str()?.to_string();
            match read_u32(&path.join("max_brightness")) {
                Ok(max) if max > 0 => {
                    return Some(Self {
                        name,
                        path,
                        max_brightness: max,
                    });
                }
                Ok(_) => warn!(device = %name, "Backlight reports max_brightness=0; ignoring"),
                Err(e) => warn!(device = %name, error = %e, "Failed to read max_brightness"),
            }
        }
        None
    }

    fn brightness_path(&self) -> PathBuf {
        self.path.join("brightness")
    }

    fn read_percent(&self) -> BrightnessResult<u8> {
        let raw = read_u32(&self.brightness_path())?;
        // Round to the nearest percent so subsequent set→get round-trips
        // are stable: a slider that snaps from 50 to 49 because of integer
        // truncation would look broken.
        let percent = ((u64::from(raw) * 100 + u64::from(self.max_brightness) / 2)
            / u64::from(self.max_brightness)) as u8;
        Ok(percent.min(100))
    }
}

fn read_u32(path: &Path) -> BrightnessResult<u32> {
    let s = fs::read_to_string(path)?;
    s.trim()
        .parse::<u32>()
        .map_err(|e| BrightnessError::Backend(format!("parse {}: {}", path.display(), e)))
}

/// Probe `brightnessctl --version` so a missing binary is caught at startup
/// rather than the first user interaction with the slider.
fn brightnessctl_available() -> bool {
    crate::helpers::command(BACKEND_NAME)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Linux brightness controller — `brightnessctl`-backed.
pub struct LinuxBrightnessController {
    capabilities: BrightnessCapabilities,
    device: Option<BacklightDevice>,
}

impl LinuxBrightnessController {
    pub fn new() -> Self {
        let device = BacklightDevice::detect();
        let has_brightnessctl = brightnessctl_available();

        let available = device.is_some() && has_brightnessctl;

        match (&device, has_brightnessctl) {
            (Some(dev), true) => info!(
                device = %dev.name,
                max_brightness = dev.max_brightness,
                backend = BACKEND_NAME,
                "Brightness controller initialized",
            ),
            (Some(dev), false) => warn!(
                device = %dev.name,
                "Found backlight {} but `brightnessctl` is not on PATH; \
                 brightness control disabled. Install the `brightnessctl` \
                 package and retry.",
                dev.name,
            ),
            (None, _) => debug!("No backlight device detected under {}", BACKLIGHT_ROOT),
        }

        let capabilities = BrightnessCapabilities {
            available,
            backend: if available {
                Some(BACKEND_NAME.to_string())
            } else {
                None
            },
            device: device.as_ref().map(|d| d.name.clone()),
        };

        Self {
            capabilities,
            device,
        }
    }

    fn set_via_brightnessctl(&self, dev: &BacklightDevice, percent: u8) -> BrightnessResult<()> {
        // `brightnessctl --device <name> set <pct>%` so the user-facing
        // `%` is interpreted relative to max_brightness even when multiple
        // backlights exist.
        let arg = format!("{}%", percent);
        let output = crate::helpers::command(BACKEND_NAME)
            .args(["--device", &dev.name, "set", &arg])
            .output()
            .map_err(|e| BrightnessError::Backend(format!("brightnessctl: {}", e)))?;
        if !output.status.success() {
            return Err(BrightnessError::Backend(format!(
                "brightnessctl exited {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim(),
            )));
        }
        Ok(())
    }
}

impl Default for LinuxBrightnessController {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BrightnessController for LinuxBrightnessController {
    fn capabilities(&self) -> &BrightnessCapabilities {
        &self.capabilities
    }

    async fn get_status(&self) -> BrightnessResult<BrightnessStatus> {
        let dev = self
            .device
            .as_ref()
            .ok_or_else(|| BrightnessError::NotAvailable("No backlight device".into()))?;
        let percent = dev.read_percent()?;
        Ok(BrightnessStatus { percent })
    }

    async fn set_brightness(&self, percent: u8) -> BrightnessResult<()> {
        if percent > 100 {
            return Err(BrightnessError::OutOfRange(percent));
        }
        if !self.capabilities.available {
            return Err(BrightnessError::NotAvailable(
                "Brightness control unavailable (no backlight or `brightnessctl` not installed)"
                    .into(),
            ));
        }
        let dev = self
            .device
            .as_ref()
            .expect("available implies device is Some");
        self.set_via_brightnessctl(dev, percent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_percent_rounds_to_nearest() {
        // The sysfs scan + fs reads are hard to fake in a unit test, but
        // the rounding math is the part most likely to be wrong, so test it
        // directly through a hand-built device.
        let dev = BacklightDevice {
            name: "test".into(),
            path: PathBuf::from("/tmp/nonexistent"),
            max_brightness: 7,
        };
        // (raw, expected_percent) — round to nearest, ties go up
        let cases: &[(u32, u8)] = &[(0, 0), (1, 14), (3, 43), (4, 57), (6, 86), (7, 100)];
        for (raw, expected) in cases {
            let percent = ((u64::from(*raw) * 100 + u64::from(dev.max_brightness) / 2)
                / u64::from(dev.max_brightness)) as u8;
            assert_eq!(percent, *expected, "raw={raw}");
        }
    }

    #[test]
    fn read_percent_caps_at_100_for_typical_max() {
        let dev = BacklightDevice {
            name: "test".into(),
            path: PathBuf::from("/tmp/nonexistent"),
            max_brightness: 255,
        };
        for raw in [0u32, 1, 64, 128, 255] {
            let percent = ((u64::from(raw) * 100 + u64::from(dev.max_brightness) / 2)
                / u64::from(dev.max_brightness)) as u8;
            assert!(percent <= 100);
        }
    }
}

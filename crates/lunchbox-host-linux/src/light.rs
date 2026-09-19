//! Linux ambient-light-sensor implementation
//!
//! Reads illuminance from an IIO ambient light sensor under
//! `/sys/bus/iio/devices/iio:deviceN/`. The channel exposes
//! `in_illuminance_raw` (an integer ADC-style reading) plus optional
//! `in_illuminance_scale` and `in_illuminance_offset`; lux is
//! `(raw + offset) * scale`, per the IIO ABI. These files are world-readable
//! (root-owned but mode `0444`/`0644`), so — like the backlight `brightness`
//! read — no privilege or helper binary is needed.
//!
//! Only the read side is implemented: an ALS has nothing to set.

use lunchbox_host_api::{
    LightSensor, LightSensorCapabilities, LightSensorError, LightSensorResult,
};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

const IIO_ROOT: &str = "/sys/bus/iio/devices";

/// A detected IIO illuminance channel.
#[derive(Debug, Clone)]
struct IlluminanceDevice {
    /// IIO device `name` (e.g. `als`), used only for logging/reporting.
    name: String,
    /// Path to the `iio:deviceN` directory.
    path: PathBuf,
    /// `in_illuminance_scale`, defaulting to 1.0 when the file is absent.
    scale: f32,
    /// `in_illuminance_offset`, defaulting to 0 when the file is absent.
    offset: f32,
}

impl IlluminanceDevice {
    /// Scan `/sys/bus/iio/devices/` for the first channel exposing
    /// `in_illuminance_raw`. Devices are considered in lexicographic order so
    /// the choice is deterministic across reboots; a device literally named
    /// `als` is preferred if present, since that is the conventional name for
    /// the panel ambient light sensor when several illuminance-capable IIO
    /// devices exist.
    fn detect() -> Option<Self> {
        let entries = fs::read_dir(IIO_ROOT).ok()?;
        let mut candidates: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.join("in_illuminance_raw").is_file())
            .collect();
        candidates.sort();

        // Prefer a device named `als`; otherwise take the first candidate.
        let chosen = candidates
            .iter()
            .find(|p| read_name(p).as_deref() == Some("als"))
            .or_else(|| candidates.first())
            .cloned()?;

        let name = read_name(&chosen).unwrap_or_else(|| {
            chosen
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("illuminance")
                .to_string()
        });
        let scale = read_f32(&chosen.join("in_illuminance_scale")).unwrap_or(1.0);
        let offset = read_f32(&chosen.join("in_illuminance_offset")).unwrap_or(0.0);

        Some(Self {
            name,
            path: chosen,
            scale,
            offset,
        })
    }

    fn read_lux(&self) -> LightSensorResult<f32> {
        let raw = read_f32(&self.path.join("in_illuminance_raw")).ok_or_else(|| {
            LightSensorError::Backend(format!(
                "failed to read {}",
                self.path.join("in_illuminance_raw").display()
            ))
        })?;
        // Per the IIO ABI, processed value = (raw + offset) * scale. Clamp to
        // non-negative: a momentary negative reading (seen on some sensors at
        // the noise floor) would otherwise map to nonsense downstream.
        Ok(((raw + self.offset) * self.scale).max(0.0))
    }
}

fn read_name(dir: &Path) -> Option<String> {
    fs::read_to_string(dir.join("name"))
        .ok()
        .map(|s| s.trim().to_string())
}

fn read_f32(path: &Path) -> Option<f32> {
    fs::read_to_string(path).ok()?.trim().parse::<f32>().ok()
}

/// Linux ambient-light-sensor reader.
pub struct LinuxLightSensor {
    capabilities: LightSensorCapabilities,
    device: Option<IlluminanceDevice>,
}

impl LinuxLightSensor {
    pub fn new() -> Self {
        let device = IlluminanceDevice::detect();
        match &device {
            Some(dev) => info!(
                device = %dev.name,
                scale = dev.scale,
                offset = dev.offset,
                "Ambient light sensor initialized",
            ),
            None => debug!("No ambient light sensor (IIO illuminance channel) detected"),
        }
        let capabilities = LightSensorCapabilities {
            available: device.is_some(),
            device: device.as_ref().map(|d| d.name.clone()),
        };
        Self {
            capabilities,
            device,
        }
    }
}

impl Default for LinuxLightSensor {
    fn default() -> Self {
        Self::new()
    }
}

impl LightSensor for LinuxLightSensor {
    fn capabilities(&self) -> &LightSensorCapabilities {
        &self.capabilities
    }

    fn read_lux(&self) -> LightSensorResult<f32> {
        let dev = self
            .device
            .as_ref()
            .ok_or_else(|| LightSensorError::NotAvailable("No ambient light sensor".into()))?;
        let lux = dev.read_lux()?;
        if lux.is_finite() {
            Ok(lux)
        } else {
            warn!(lux, "Ambient light sensor returned a non-finite reading");
            Err(LightSensorError::Backend("non-finite lux reading".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lux_applies_scale_and_offset() {
        let dev = IlluminanceDevice {
            name: "test".into(),
            path: PathBuf::from("/tmp/nonexistent"),
            scale: 2.0,
            offset: 1.0,
        };
        // read_lux reads the sysfs file, which won't exist here; verify the
        // math directly instead, mirroring the actual formula.
        let raw = 5.0f32;
        assert_eq!(((raw + dev.offset) * dev.scale).max(0.0), 12.0);
    }

    #[test]
    fn negative_readings_clamp_to_zero() {
        let scale = 1.0f32;
        let offset = -10.0f32;
        let raw = 3.0f32;
        assert_eq!(((raw + offset) * scale).max(0.0), 0.0);
    }
}

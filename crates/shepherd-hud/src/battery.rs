//! Battery monitoring module
//!
//! Monitors battery status via sysfs or UPower D-Bus interface.

use std::fs;
use std::path::Path;

/// Battery status
#[derive(Debug, Clone, Default)]
pub struct BatteryStatus {
    /// Battery percentage (0-100)
    pub percent: Option<u8>,
    /// Whether the battery is charging
    pub charging: bool,
    /// Whether AC power is connected
    pub ac_connected: bool,
}

impl BatteryStatus {
    /// Read battery status from sysfs
    pub fn read() -> Self {
        let mut status = BatteryStatus::default();

        // Try to find a battery in /sys/class/power_supply
        let power_supply = Path::new("/sys/class/power_supply");
        if !power_supply.exists() {
            return status;
        }

        if let Ok(entries) = fs::read_dir(power_supply) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name();
                let name_str = name.to_string_lossy();

                // Check for battery
                if name_str.starts_with("BAT")
                    && let Some((percent, charging)) = read_battery_info(&path)
                {
                    status.percent = Some(percent);
                    status.charging = charging;
                }

                // Check for AC adapter
                if (name_str.starts_with("AC") || name_str.contains("ADP"))
                    && let Some(online) = read_ac_status(&path)
                {
                    status.ac_connected = online;
                }
            }
        }

        status
    }

    /// Get an icon name for the current battery status
    /// See https://github.com/GNOME/adwaita-icon-theme/tree/master/Adwaita/symbolic/status
    pub fn icon_name(&self) -> &'static str {
        let Some(p) = self.percent else {
            return "battery-missing-symbolic";
        };

        // Round down to nearest 10, capped at 100; use as array index (0–10).
        let idx = ((p.min(100) / 10) as usize).min(10);

        if self.charging {
            const ICONS: [&str; 11] = [
                "battery-level-0-charging-symbolic",
                "battery-level-10-charging-symbolic",
                "battery-level-20-charging-symbolic",
                "battery-level-30-charging-symbolic",
                "battery-level-40-charging-symbolic",
                "battery-level-50-charging-symbolic",
                "battery-level-60-charging-symbolic",
                "battery-level-70-charging-symbolic",
                "battery-level-80-charging-symbolic",
                "battery-level-90-charging-symbolic",
                "battery-level-100-charged-symbolic",
            ];
            ICONS[idx]
        } else if self.ac_connected {
            const ICONS: [&str; 11] = [
                "battery-level-0-plugged-in-symbolic",
                "battery-level-10-plugged-in-symbolic",
                "battery-level-20-plugged-in-symbolic",
                "battery-level-30-plugged-in-symbolic",
                "battery-level-40-plugged-in-symbolic",
                "battery-level-50-plugged-in-symbolic",
                "battery-level-60-plugged-in-symbolic",
                "battery-level-70-plugged-in-symbolic",
                "battery-level-80-plugged-in-symbolic",
                "battery-level-90-plugged-in-symbolic",
                "battery-level-100-plugged-in-symbolic",
            ];
            ICONS[idx]
        } else {
            const ICONS: [&str; 11] = [
                "battery-level-0-symbolic",
                "battery-level-10-symbolic",
                "battery-level-20-symbolic",
                "battery-level-30-symbolic",
                "battery-level-40-symbolic",
                "battery-level-50-symbolic",
                "battery-level-60-symbolic",
                "battery-level-70-symbolic",
                "battery-level-80-symbolic",
                "battery-level-90-symbolic",
                "battery-level-100-symbolic",
            ];
            ICONS[idx]
        }
    }

    /// Check if battery is critically low
    #[allow(dead_code)]
    pub fn is_critical(&self) -> bool {
        matches!(self.percent, Some(p) if p < 10 && !self.charging)
    }
}

fn read_battery_info(path: &Path) -> Option<(u8, bool)> {
    // Read capacity
    let capacity_path = path.join("capacity");
    let capacity: u8 = fs::read_to_string(&capacity_path)
        .ok()?
        .trim()
        .parse()
        .ok()?;

    // Read status
    let status_path = path.join("status");
    let status = fs::read_to_string(&status_path).ok()?;
    let charging = status.trim().eq_ignore_ascii_case("charging")
        || status.trim().eq_ignore_ascii_case("full");

    Some((capacity.min(100), charging))
}

fn read_ac_status(path: &Path) -> Option<bool> {
    let online_path = path.join("online");
    let online = fs::read_to_string(&online_path).ok()?;
    Some(online.trim() == "1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_battery_icon_names() {
        let status = BatteryStatus {
            percent: Some(95),
            charging: false,
            ac_connected: false,
        };
        assert_eq!(status.icon_name(), "battery-level-90-symbolic");

        let status = BatteryStatus {
            percent: Some(50),
            charging: true,
            ac_connected: true,
        };
        assert_eq!(status.icon_name(), "battery-level-50-charging-symbolic");

        let status = BatteryStatus {
            percent: Some(5),
            charging: false,
            ac_connected: false,
        };
        assert_eq!(status.icon_name(), "battery-level-0-symbolic");
        assert!(status.is_critical());

        let status = BatteryStatus {
            percent: Some(100),
            charging: true,
            ac_connected: true,
        };
        assert_eq!(status.icon_name(), "battery-level-100-charged-symbolic");

        let status = BatteryStatus {
            percent: Some(60),
            charging: false,
            ac_connected: true,
        };
        assert_eq!(status.icon_name(), "battery-level-60-plugged-in-symbolic");

        let status = BatteryStatus {
            percent: None,
            charging: false,
            ac_connected: false,
        };
        assert_eq!(status.icon_name(), "battery-missing-symbolic");
    }
}

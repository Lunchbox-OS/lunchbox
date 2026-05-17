//! gilrs initialization and connected-controller logging.
//!
//! The bridge uses gilrs (the same library `shepherd-launcher-ui` already
//! uses) for gamepad enumeration, hotplug, and event reading. The preset
//! layer consumes `gilrs::Button` / `gilrs::Axis` directly, so this module
//! is intentionally thin — no wrapper types, just startup helpers.

use gilrs::Gilrs;
use tracing::warn;

pub fn init() -> anyhow::Result<Gilrs> {
    Gilrs::new().map_err(|e| anyhow::anyhow!("failed to initialize gilrs: {e}"))
}

/// Log the gamepads gilrs sees at startup. Helpful when a user reports
/// "the bridge isn't doing anything" — the log answers "did the bridge
/// see your controller at all".
pub fn log_connected(gilrs: &Gilrs) {
    let mut count = 0;
    for (id, gamepad) in gilrs.gamepads() {
        tracing::info!(
            id = ?id,
            name = %gamepad.name(),
            uuid = ?gamepad.uuid(),
            "Gamepad available"
        );
        count += 1;
    }
    if count == 0 {
        warn!("No gamepads connected yet — bridge will activate when one is plugged in");
    }
}

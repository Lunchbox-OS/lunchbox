//! Ambient-light-sensor interface
//!
//! Defines the capability-based interface for reading an ambient light
//! sensor (ALS), used by the automatic-brightness feature. Kept deliberately
//! small — a single scalar lux read — because that is all the auto-brightness
//! loop needs. Unlike [`crate::brightness`], there is no "set": a light
//! sensor is read-only.
//!
//! The read is synchronous: an ALS reading is a single small sysfs read, so
//! there is nothing to gain from making the trait async, and keeping it sync
//! lets the pure auto-brightness logic be exercised without a runtime.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors from ambient-light-sensor reads
#[derive(Debug, Error)]
pub enum LightSensorError {
    #[error("Ambient light sensor not available: {0}")]
    NotAvailable(String),

    #[error("Backend error: {0}")]
    Backend(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type LightSensorResult<T> = Result<T, LightSensorError>;

/// Ambient-light-sensor capabilities
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LightSensorCapabilities {
    /// Whether an ambient light sensor is available
    pub available: bool,
    /// Name of the IIO device being read (e.g. `als`), if any
    pub device: Option<String>,
}

/// Ambient-light-sensor reader — implemented by platform-specific adapters.
pub trait LightSensor: Send + Sync {
    /// Get the capabilities of this light sensor
    fn capabilities(&self) -> &LightSensorCapabilities;

    /// Read the current ambient illuminance, in lux.
    fn read_lux(&self) -> LightSensorResult<f32>;
}

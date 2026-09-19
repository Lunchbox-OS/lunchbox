//! Host adapter trait interfaces for lunchboxd
//!
//! This crate defines the capability-based interface between the lunchboxd service
//! and platform-specific implementations. It contains no platform code itself.

mod brightness;
mod capabilities;
mod handle;
mod light;
mod mock;
mod network;
mod traits;
mod volume;

pub use brightness::*;
pub use capabilities::*;
pub use handle::*;
pub use light::*;
pub use mock::*;
pub use network::*;
pub use traits::*;
pub use volume::*;

//! Shared output backend for the input-compat sidecars.
//!
//! The bridges read real devices and synthesize mouse + keyboard events. This
//! crate owns the *output* half: a backend-agnostic event vocabulary
//! ([`OutputEvent`]), the sink trait the bridge loops drive ([`OutputSink`]),
//! and a `/dev/uinput`-backed implementation ([`UinputSink`]) that works on
//! any Wayland compositor (and X11), unlike the wlroots-only Wayland protocols
//! the bridges used before (issue #58).

mod event;
mod sink;
mod uinput;

pub use event::{OutputEvent, ScrollAxis};
pub use sink::OutputSink;
pub use uinput::UinputSink;

//! Shared egui UI for browsing a shepherd-media library.
//!
//! Both the Linux binary (`shepherd-media`) and the Android app
//! (`shepherd-media-android`) render the same poster grid for viewing a
//! library's contents. This crate is the platform-agnostic home for that view
//! (and its theme), so the two front-ends stay visually and behaviourally in
//! sync. It depends only on `egui` and `shepherd-media-core` — input handling,
//! poster fetching, and playback stay in each binary.
//!
//! The `egui::Image` loader (`egui_extras::install_image_loaders`) must be
//! installed by the binary that owns the `egui::Context`.

pub mod grid;
pub mod theme;

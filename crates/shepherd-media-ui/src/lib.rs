//! Shared egui UI for browsing a shepherd-media library.
//!
//! Both the Linux binary (`shepherd-media`) and the Android app
//! (`shepherd-media-android`) render the same poster grid for viewing a
//! library's contents. This crate is the platform-agnostic home for that view
//! (and its theme), so the two front-ends stay visually and behaviourally in
//! sync. Beyond the browse grid it also hosts the shared video compositor (see
//! [`video`]) that both front-ends use to draw the player's GL output; the
//! transport controls, theme, and playback driver stay in each binary. It
//! depends on `egui` and `glow` (and `shepherd-media-core`) but not `eframe`.
//!
//! The `egui::Image` loader (`egui_extras::install_image_loaders`) must be
//! installed by the binary that owns the `egui::Context`.

pub mod grid;
pub mod theme;
pub mod video;

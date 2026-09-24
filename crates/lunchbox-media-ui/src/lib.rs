//! Shared egui UI for the lunchbox-media library screen and player.
//!
//! Both the Linux binary (`lunchbox-media`) and the Android app
//! (`lunchbox-media-android`) show the same library screen and the same
//! transport controls, in the Lunchbox branding. This crate is the
//! platform-agnostic home for those views and their theme, so the two
//! front-ends stay visually and behaviourally in sync. Beyond them it also
//! hosts the shared video compositor (see [`video`]) that the Linux binary
//! draws the player's GL output with; the playback driver stays in each
//! binary. It depends on `egui` and `glow` (and `lunchbox-media-core`) but not
//! `eframe`.
//!
//! The `egui::Image` loader (`egui_extras::install_image_loaders`) must be
//! installed by the binary that owns the `egui::Context`, and
//! [`theme::install_fonts`] (or [`theme::install`]) called on it, since every
//! view here sets its type in Baloo 2.

pub mod library;
pub mod theme;
pub mod video;

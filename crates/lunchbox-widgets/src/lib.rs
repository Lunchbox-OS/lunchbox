//! GTK4 widgets that more than one lunchbox surface draws.
//!
//! The launcher (`lunchbox-launcher-ui`) and the HUD (`lunchbox-hud`) are
//! separate binaries with separate stylesheets, and almost nothing they show is
//! shared — deliberately, because they are looked at from different distances
//! and answer different questions. What lands here is the exception: a widget
//! both of them draw, where two copies would drift apart.
//!
//! Everything in here is *drawn*, not styled, so a widget takes its size as a
//! number and its colour from CSS. See `clock_face` for what that means in
//! practice.

pub mod clock_face;

pub use clock_face::ClockFace;

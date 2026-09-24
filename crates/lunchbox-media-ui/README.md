# lunchbox-media-ui

Shared egui UI for `lunchbox-media`: the library screen (a library's
thumbnails, and the "keep watching" row above them) and its theme, in the
Lunchbox branding.

Both front-ends render the same view from it:

- the Linux binary [`lunchbox-media`](../lunchbox-media) (`ui/mod.rs`), and
- the Android app [`lunchbox-media-android`](../lunchbox-media-android)
  (`ui.rs`).

Keeping the view here means the two stay visually and behaviourally in sync
instead of drifting apart.

## Scope

- `library` — the library screen, drawn to the design in issue #224: one sunk
  compartment on the enamel field, holding 16:9 thumbnails in rows that fill
  column by column and spill to the right, so the library scrolls sideways and
  never down. A thumbnail carries a duration chip and, when it has been
  started, a progress strip. When the item watched last was left partway
  through, a "keep watching" row sits above a one-row library, with its
  Continue button focused first. `LibraryView` owns the focus (a D-pad model
  over the column-filled grid, tested in the module) and the sideways scroll
  (following the focus, dragged with a kinetic flick, and paged by the chevron
  chips at an overflowing edge). `draw(...)` returns the id of an item to play;
  it does not own posters, playback or resume state.
- `theme` — the palette (read from `assets/branding/tokens.json` through
  [`lunchbox-branding`](../lunchbox-branding)), Baloo 2 at the two weights the
  design uses, the `Scale` every measurement goes through, and the shapes the
  screens are drawn from: the sunk compartment, the selection fill, progress
  bars, round buttons and the media glyphs.
- `video` — the transport overlay both players draw, and the compositor the
  Linux binary draws video through.

## Platform-agnostic by construction

Depends only on [`egui`], `glow`, `lunchbox-branding` and `lunchbox-media-core`
(for `Item` / `resolve_source` / `PlatformInfo`). The caller supplies:

- the item list, the item to offer to continue (if any), a
  `&dyn Fn(&str) -> Option<Vec<u8>>` returning already-fetched encoded poster
  bytes, and a `&dyn Fn(&Item) -> Option<f32>` saying how much of an item has
  been watched;
- its own gamepad, translated into `LibraryView::navigate` / `activate` (the
  keyboard, which is what a TV remote's D-pad arrives as, is read by the view);
- the `egui_extras` image loader, installed on the `egui::Context` by the binary
  (`egui_extras::install_image_loaders`), since this crate doesn't pull it in;
- `theme::install_fonts` (or `theme::install`) on that context, since the views
  name Baloo 2 and egui panics on a font family nobody registered.

Playback, poster caching, resume state, connectivity, and process/JNI concerns
stay in each binary.

## Measurements

Every length is in the design's pixels, at the 1280×664 field the mockups were
drawn on, and multiplied by a `Scale` fitted to the space the view is given
(the narrower axis, like the launcher). The media-specific numbers — 340px
thumbnails, two rows, the hero's 372px thumbnail — are constants at the top of
`library.rs`, from the brief reproduced in
`docs/ai/history/2026-09-23 001 media-app-reskin (#224).md`.

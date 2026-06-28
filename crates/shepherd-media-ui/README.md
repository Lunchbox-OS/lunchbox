# shepherd-media-ui

Shared egui UI for browsing a `shepherd-media` library — the poster grid used
for "view the contents of a library" — plus its theme.

Both front-ends render the same view from it:

- the Linux binary [`shepherd-media`](../shepherd-media) (`ui/mod.rs`), and
- the Android app [`shepherd-media-android`](../shepherd-media-android)
  (`ui.rs`).

Keeping the grid here means the two stay visually and behaviourally in sync
instead of drifting apart.

## Scope

- `grid` — the responsive poster grid: tiles sized for a 10-foot UI, posters
  letterboxed to their aspect, an explicit focused-tile highlight, custom
  drag-to-scroll with kinetic flick, and `scroll_to_me` follow on focus change.
  `draw(...)` returns the id of an item the user activated; it does not own
  selection, input, poster fetching, or playback.
- `theme` — the dark browse palette and a `theme::install` helper.

## Platform-agnostic by construction

Depends only on [`egui`] and `shepherd-media-core` (for `Item` /
`resolve_source` / `PlatformInfo`). The caller supplies:

- the item list and a `&mut focused` index (moved by its own keyboard / D-pad /
  gamepad handling — the tiles are custom-painted and don't use egui's own
  focus),
- a `&dyn Fn(&str) -> Option<Vec<u8>>` returning already-fetched encoded poster
  bytes, and
- the `egui_extras` image loader, installed on the `egui::Context` by the binary
  (`egui_extras::install_image_loaders`), since this crate doesn't pull it in.

Playback, poster caching, connectivity, and process/JNI concerns stay in each
binary.

# shepherd-media

Linux binary for the `shepherd-media` library-launcher. Wraps
`shepherd-media-core` with:

- A `clap`-based CLI exposing `validate`, `play`, and `browse` subcommands.
- An `egui` + `eframe` shell that hosts both the browse-mode poster grid
  and a touch- and controller-friendly playback view. mpv is composited
  into the same window via `libmpv2`'s OpenGL render context, so a
  single surface owns all input (touch, mouse, keyboard, gamepad).
- An asynchronous poster prefetcher that keeps the network out of the core.

Library files are TOML; see `docs/shepherd-media.md` for the schema and example
authoring patterns.

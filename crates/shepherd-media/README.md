# shepherd-media

Linux binary for the `shepherd-media` library-launcher. Wraps
`shepherd-media-core` with:

- A `clap`-based CLI exposing `validate`, `play`, and `browse` subcommands.
- An `egui` + `eframe` shell that hosts both the browse-mode poster grid
  and a touch- and controller-friendly playback view. mpv is composited
  into the same window via `libmpv2`'s OpenGL render context, so a
  single surface owns all input (touch, mouse, keyboard, gamepad).
- An asynchronous poster prefetcher that keeps the network out of the core.
- The `--resume` option (default off): per-library playback positions kept under
  `$XDG_STATE_HOME/shepherd/media/resume/`, so an item re-opens where it stopped
  and browse mode offers to continue the last one watched. The state model is
  shared with the Android app (`shepherd-media-app`'s `resume` module); see
  `docs/shepherd-media.md`.

Library files are TOML; see `docs/shepherd-media.md` for the schema and example
authoring patterns.

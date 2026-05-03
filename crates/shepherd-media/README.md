# shepherd-media

Linux binary for the `shepherd-media` library-launcher. Wraps
`shepherd-media-core` with:

- A `clap`-based CLI exposing `validate`, `play`, and `browse` subcommands.
- An `egui` + `eframe` poster grid for browse mode, with keyboard and gamepad
  navigation.
- An asynchronous poster prefetcher that keeps the network out of the core.

Library files are TOML; see `docs/shepherd-media.md` for the schema and example
authoring patterns.

# lunchbox-media

Linux binary for the `lunchbox-media` library-launcher. Wraps
`lunchbox-media-core` with:

- A `clap`-based CLI exposing `validate`, `play`, and `browse` subcommands.
- An `egui` + `eframe` shell that hosts both the browse-mode poster grid
  and a touch- and controller-friendly playback view. mpv is composited
  into the same window via `libmpv2`'s OpenGL render context, so a
  single surface owns all input (touch, mouse, keyboard, gamepad).
- An asynchronous poster prefetcher that keeps the network out of the core.
- `CachingPlayer`, the player-side half of the video cache: it substitutes a
  cached file for a remote source at play time and queues an uncached item for
  download once it is watched to the end. The cache itself lives in
  `lunchbox-media-cache`, which lunchboxd shares.
- `SkipWatcher` (`skipping.rs`), which skips SponsorBlock segments in YouTube
  videos when a parent has enabled it (`--sponsorblock-categories`, filled in by
  lunchboxd from `service.media.sponsorblock`). It owns only the timing — an
  off-thread lookup, a duration mpv does not know until the file is open, and a
  per-frame position; the decisions are `lunchbox-media-core`'s. With no
  categories there is no watcher and nothing reaches the network.
- The `--resume` option (default off): per-library playback positions kept under
  `$XDG_STATE_HOME/lunchbox/media/resume/`, so an item re-opens where it stopped
  and browse mode offers to continue the last one watched. The state model is
  shared with the Android app (`lunchbox-media-app`'s `resume` module); see
  `docs/lunchbox-media.md`.

Library files are TOML; see `docs/lunchbox-media.md` for the schema and example
authoring patterns.

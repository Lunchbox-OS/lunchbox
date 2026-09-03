# shepherd-media-core

Platform-agnostic core for `shepherd-media`. Contains:

- **Library file** parsing and validation (`library.rs`, `schema.rs`).
- **URI classification** with a static DRM/subscription rejection list (`uri.rs`).
- **Source resolution** for the running platform (`resolver.rs`).
- The **`PlayerHandle` trait** that abstracts the playback backend, plus a
  `LibmpvPlayer` implementation gated behind the `libmpv` feature
  (`player.rs`). Playback can be started at an offset
  (`set_start_position`) — a per-file option on the load rather than a seek
  afterwards — which is what the front-ends' opt-in resume feature rides on.
- A **session state machine** that drives `Browsing` ↔ `Playing` transitions
  (`session.rs`).
- **SponsorBlock** (`sponsorblock.rs`) — the wire format, the filtering that
  decides which submissions apply to the file actually loaded, and the state
  machine that says when to seek (issue #159). Pure: both front-ends fetch their
  own way and then reach the same decision here, which is what keeps a video
  from skipping differently depending on which one is playing it.
- The **stdout line protocol** that the platform binary uses to report
  playback events to `shepherdd` (`protocol.rs`).

This crate intentionally has no dependency on Wayland, X11, GTK, egui, or any
process-spawning utility, and performs no network I/O. Those concerns belong
to platform binaries (see `shepherd-media` for the Linux build).

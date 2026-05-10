# YouTube playlist as library source

**Branch:** `u/albert/9/media-launcher`  
**Issues:** #9 (media type and libraries), #34 (PR: Media launcher and libraries)

## Summary

Added support for passing a YouTube playlist URL directly as the `--library`
argument to `shepherd-media`, instead of requiring a TOML or M3U file.

```sh
shepherd-media browse --library "https://www.youtube.com/playlist?list=PL…"
shepherd-media validate "https://www.youtube.com/playlist?list=PL…"
shepherd-media play --library "https://www.youtube.com/playlist?list=PL…" --item <item-id>
```

`yt-dlp` must be installed at runtime; `shepherd-media` fails fast with a
clear install message if it is absent.

## Files changed

### New
- `crates/shepherd-media-core/src/youtube_playlist.rs` — URL detection
  (`is_youtube_playlist_url`), `YoutubePlaylistEntry` struct, and
  `build_library_from_entries` (pure, no I/O; [android-portability] safe).
- `crates/shepherd-media/src/youtube.rs` — invokes
  `yt-dlp --dump-json --flat-playlist` as a subprocess and parses the
  newline-delimited JSON into `Vec<YoutubePlaylistEntry>`.

### Modified
- `crates/shepherd-media-core/src/lib.rs` — exports the new module.
- `crates/shepherd-media/src/cli.rs` — `library` field changed from
  `PathBuf` to `String` in all three subcommands.
- `crates/shepherd-media/src/main.rs` — adds `load_library_from_source`
  helper that dispatches on URL vs. file path.
- `crates/shepherd-media/Cargo.toml` — adds `serde` and `serde_json`
  workspace deps (needed by `youtube.rs` to deserialize yt-dlp output).

## Design decisions

- **No I/O in the core.** `YoutubePlaylistEntry` and `build_library_from_entries`
  live in `shepherd-media-core` for Android portability; the subprocess call
  lives only in the Linux binary.
- **Detection is URL-based.** `is_youtube_playlist_url` checks for a `list=`
  query parameter on a YouTube host. A plain watch URL without `list=` is not
  treated as a library source.
- **Item IDs from video IDs.** YouTube video IDs (`[A-Za-z0-9_-]`) are
  sanitized to `[a-z0-9-]+` for use as item IDs. The sanitization lowercases
  letters and collapses non-alphanumeric runs to a single dash.
- **Library ID from playlist ID.** The `list=` value (e.g. `PLrEnWoR732-…`)
  is sanitized the same way to produce the `library_id`.
- **Thumbnails as posters.** `yt-dlp` provides per-video thumbnail URLs;
  these become `PosterRef::Remote` entries and are fetched asynchronously by
  the existing poster prefetch machinery.
- **yt-dlp not a Rust dep.** Consistent with the spec's prohibition on adding
  yt-dlp as a direct dependency.

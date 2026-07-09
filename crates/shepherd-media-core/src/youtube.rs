//! Shared YouTube / yt-dlp knobs used by every path that resolves or downloads
//! a video stream, so they behave identically.

/// The yt-dlp `--extractor-args` value routing YouTube video access through the
/// `android_vr` + `android` player clients.
///
/// `android_vr` serves DASH video formats without a PO token (the default
/// web/tv clients increasingly gate those), and `android` additionally exposes
/// the legacy progressive itag 18 (360p H.264, non-DRM) that DRM-protected
/// "full episode" uploads fall back to — the default clients report those as
/// "not available". The `ytdl-format` muxed fallback (`/best`, or `/b` on
/// Android) then selects itag 18 for such videos while normal videos keep their
/// higher-quality DASH streams.
///
/// Used by the Android stream resolver, mpv's `ytdl_hook` on the Linux build,
/// and the Linux video-file cache. When passed to mpv's `ytdl-raw-options`
/// (a comma-separated key/value list) the value must be length-prefix quoted so
/// the comma between the two client names isn't treated as a list separator.
pub const YOUTUBE_EXTRACTOR_ARGS: &str = "youtube:player_client=android_vr,android";

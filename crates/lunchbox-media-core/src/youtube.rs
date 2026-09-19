//! Shared YouTube / yt-dlp knobs used by every path that resolves or downloads
//! a video stream, so they behave identically.

/// The yt-dlp `--extractor-args` value used by every path that resolves or
/// downloads a YouTube stream.
///
/// `default` is yt-dlp's own client list, which is what serves working DASH
/// formats for an ordinary video. `android` is *added* to it rather than
/// replacing it, for one reason: it exposes the legacy progressive itag 18
/// (360p H.264, non-DRM) that DRM-protected "full episode" uploads fall back
/// to, and which the default clients report as "not available". Format
/// selection then sorts it out per video — a normal upload takes its DASH
/// streams, while a DRM one finds nothing matching and lands on the muxed
/// fallback (`/best`, or `/b` on Android), which is itag 18.
///
/// This previously read `player_client=android_vr,android`, which *replaced*
/// the defaults. `android_vr` served DASH without a PO token at the time, and
/// that mattered because the default clients were gating those. YouTube has
/// since started rejecting `android_vr`'s media URLs with HTTP 403 partway
/// through the transfer, so the setting inverted: DRM full episodes still
/// worked, via itag 18, and every ordinary video failed. Keeping `android`
/// alongside the defaults preserves what the old value was added for without
/// betting the common case on one non-default client.
///
/// Used by the Android stream resolver, mpv's `ytdl_hook` on the Linux build,
/// and the Linux video-file cache. When passed to mpv's `ytdl-raw-options`
/// (a comma-separated key/value list) the value must be length-prefix quoted so
/// the comma between the two client names isn't treated as a list separator.
pub const YOUTUBE_EXTRACTOR_ARGS: &str = "youtube:player_client=default,android";

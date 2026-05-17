//! URI classification and DRM/subscription rejection.
//!
//! Adding entries to the rejection list is a one-line change; do not try to
//! be clever about pattern matching here.

use thiserror::Error;
use url::Url;

use crate::library::ClassifiedUri;

/// Hosts whose content is locked behind DRM/subscription paywalls.
///
/// These are rejected at validation time, not playback time, so authors get
/// an immediate error instead of a broken poster grid.
pub const REJECTED_DRM_HOSTS: &[&str] = &[
    "netflix.com",
    "www.netflix.com",
    "disneyplus.com",
    "www.disneyplus.com",
    "hulu.com",
    "www.hulu.com",
    "max.com",
    "hbomax.com",
    "play.hbomax.com",
    "primevideo.com",
    "www.amazon.com",
    "appletv.apple.com",
    "tv.apple.com",
    "peacocktv.com",
    "www.peacocktv.com",
    "paramountplus.com",
    "www.paramountplus.com",
    "spotify.com",
    "open.spotify.com",
    "music.apple.com",
];

/// URL schemes used by various DRM protection systems.
pub const REJECTED_DRM_SCHEMES: &[&str] = &["widevine", "playready", "fairplay"];

/// Path prefixes that must be matched on `www.amazon.com` to flag a Prime
/// Video URL. Only paths under these prefixes are rejected; the bare host
/// already triggers, but this kept around for the human-readable rule string.
const AMAZON_PRIME_VIDEO_PREFIXES: &[&str] = &["/gp/video", "/Prime-Video"];

/// Recognised media-file extensions that indicate a direct HTTP(S) media URL.
const DIRECT_HTTP_EXTENSIONS: &[&str] = &[
    ".mp4", ".mkv", ".webm", ".mov", ".m4v", ".mp3", ".flac", ".opus", ".ogg", ".m4a", ".wav",
    ".m3u8", ".mpd",
];

/// YouTube-equivalent hosts. mpv's `ytdl=yes` handles these via yt-dlp.
const YOUTUBE_HOSTS: &[&str] = &[
    "youtube.com",
    "www.youtube.com",
    "m.youtube.com",
    "youtu.be",
    "youtube-nocookie.com",
];

/// A DRM/subscription-service rejection diagnostic.
///
/// Carries the rejected URI plus the matched rule so the error message can
/// tell the author exactly which entry on which line tripped the check.
#[derive(Debug, Clone)]
pub struct DrmRejection {
    pub uri: String,
    pub rule: String,
}

impl std::fmt::Display for DrmRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "URI `{}` matches DRM/subscription rule `{}`. \
             Subscription/DRM services are out of scope for shepherd-media. \
             See shepherd-launcher issues #2 and #10.",
            self.uri, self.rule
        )
    }
}

#[derive(Debug, Error)]
pub enum UriError {
    #[error("invalid URI `{uri}`: {reason}")]
    Invalid { uri: String, reason: String },

    #[error("{0}")]
    Drm(DrmRejection),
}

/// Classify a URI string into one of the categories the rest of the crate
/// understands.
///
/// Returns `Err(UriError::Drm)` for any URI that matches the rejection list,
/// `Err(UriError::Invalid)` for malformed input, and a `ClassifiedUri`
/// otherwise.
pub fn classify(input: &str) -> Result<ClassifiedUri, UriError> {
    let trimmed = input.trim();

    // Catch DRM-only schemes before parsing as URLs, since `widevine:foo` may
    // not satisfy `Url::parse` on every platform.
    if let Some(scheme_end) = trimmed.find(':') {
        let scheme = &trimmed[..scheme_end].to_ascii_lowercase();
        if REJECTED_DRM_SCHEMES.contains(&scheme.as_str()) {
            return Err(UriError::Drm(DrmRejection {
                uri: trimmed.to_string(),
                rule: format!("scheme:{scheme}"),
            }));
        }
    }

    let url = Url::parse(trimmed).map_err(|e| UriError::Invalid {
        uri: trimmed.to_string(),
        reason: e.to_string(),
    })?;

    match url.scheme() {
        "file" => classify_file(&url),
        "http" | "https" => classify_http(&url),
        other => Err(UriError::Invalid {
            uri: trimmed.to_string(),
            reason: format!("unsupported scheme `{other}`"),
        }),
    }
}

fn classify_file(url: &Url) -> Result<ClassifiedUri, UriError> {
    let path = url.to_file_path().map_err(|()| UriError::Invalid {
        uri: url.to_string(),
        reason: "file:// URI must be an absolute path with no host".into(),
    })?;
    if !path.is_absolute() {
        return Err(UriError::Invalid {
            uri: url.to_string(),
            reason: "file:// path must be absolute".into(),
        });
    }
    Ok(ClassifiedUri::Local(path))
}

fn classify_http(url: &Url) -> Result<ClassifiedUri, UriError> {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();

    if let Some(rule) = drm_match(&host, url) {
        return Err(UriError::Drm(DrmRejection {
            uri: url.to_string(),
            rule,
        }));
    }

    if YOUTUBE_HOSTS.contains(&host.as_str()) {
        return Ok(ClassifiedUri::YouTube(url.clone()));
    }

    let path_lower = url.path().to_ascii_lowercase();
    if DIRECT_HTTP_EXTENSIONS
        .iter()
        .any(|ext| path_lower.ends_with(ext))
    {
        return Ok(ClassifiedUri::DirectHttp(url.clone()));
    }

    Ok(ClassifiedUri::Unknown(url.clone()))
}

/// Extract the YouTube video ID from a YouTube URL, handling the common URL
/// shapes (`watch?v=ID`, `youtu.be/ID`, `/embed/ID`, `/v/ID`, `/shorts/ID`).
///
/// Returns `None` if the host is not a YouTube host, no ID can be located, or
/// the candidate ID contains characters outside YouTube's `[A-Za-z0-9_-]` set.
/// The validation prevents an attacker-supplied URL from injecting URL syntax
/// into the derived thumbnail URL downstream.
pub fn youtube_video_id(url: &Url) -> Option<String> {
    let host = url.host_str()?.to_ascii_lowercase();
    if !YOUTUBE_HOSTS.contains(&host.as_str()) {
        return None;
    }

    if let Some((_, v)) = url.query_pairs().find(|(k, _)| k == "v") {
        let candidate = v.into_owned();
        if is_valid_youtube_video_id(&candidate) {
            return Some(candidate);
        }
    }

    let mut segments = url.path_segments()?.filter(|s| !s.is_empty());
    let first = segments.next()?;
    let candidate = if host == "youtu.be" {
        first.to_string()
    } else if matches!(first, "embed" | "v" | "shorts") {
        segments.next()?.to_string()
    } else {
        return None;
    };

    is_valid_youtube_video_id(&candidate).then_some(candidate)
}

/// Build the standard YouTube thumbnail URL for a video ID. `hqdefault.jpg`
/// is the highest size guaranteed to exist for every video (480×360);
/// `maxresdefault.jpg` is sharper but 404s on older or low-resolution uploads.
pub fn youtube_thumbnail_url(video_id: &str) -> Option<Url> {
    if !is_valid_youtube_video_id(video_id) {
        return None;
    }
    Url::parse(&format!("https://i.ytimg.com/vi/{video_id}/hqdefault.jpg")).ok()
}

fn is_valid_youtube_video_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn drm_match(host: &str, url: &Url) -> Option<String> {
    if REJECTED_DRM_HOSTS.contains(&host) {
        // Amazon's storefront is huge; keep the rule string specific so the
        // author knows it's the Prime Video subpaths that are blocked.
        if host == "www.amazon.com" {
            let path = url.path();
            if AMAZON_PRIME_VIDEO_PREFIXES
                .iter()
                .any(|p| path.starts_with(p))
            {
                return Some(format!("host:{host}{}", path));
            }
            // Other Amazon paths are not DRM by themselves; let them pass.
            return None;
        }
        return Some(format!("host:{host}"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn local_file_is_classified() {
        match classify("file:///srv/media/movie.mp4").unwrap() {
            ClassifiedUri::Local(p) => assert_eq!(p, PathBuf::from("/srv/media/movie.mp4")),
            other => panic!("expected Local, got {other:?}"),
        }
    }

    #[test]
    fn youtube_url_is_classified() {
        let result = classify("https://www.youtube.com/watch?v=abc").unwrap();
        assert!(matches!(result, ClassifiedUri::YouTube(_)));
    }

    #[test]
    fn direct_mp4_url_is_classified() {
        let result = classify("https://example.com/video.mp4").unwrap();
        assert!(matches!(result, ClassifiedUri::DirectHttp(_)));
    }

    #[test]
    fn hls_manifest_is_direct_http() {
        let result = classify("https://example.com/stream/index.m3u8").unwrap();
        assert!(matches!(result, ClassifiedUri::DirectHttp(_)));
    }

    #[test]
    fn unknown_http_url_is_unknown() {
        let result = classify("https://example.com/some/page").unwrap();
        assert!(matches!(result, ClassifiedUri::Unknown(_)));
    }

    #[test]
    fn netflix_is_rejected() {
        let err = classify("https://www.netflix.com/watch/123").unwrap_err();
        assert!(matches!(err, UriError::Drm(_)));
    }

    #[test]
    fn widevine_scheme_is_rejected() {
        let err = classify("widevine:keys/abc123").unwrap_err();
        assert!(matches!(err, UriError::Drm(_)));
    }

    #[test]
    fn amazon_prime_path_is_rejected() {
        let err = classify("https://www.amazon.com/gp/video/detail/abc").unwrap_err();
        assert!(matches!(err, UriError::Drm(_)));
    }

    #[test]
    fn amazon_non_prime_path_is_unknown() {
        let result = classify("https://www.amazon.com/dp/B07XYZ").unwrap();
        assert!(matches!(result, ClassifiedUri::Unknown(_)));
    }

    fn parse(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn youtube_video_id_from_watch_url() {
        assert_eq!(
            youtube_video_id(&parse("https://www.youtube.com/watch?v=YE7VzlLtp-4")),
            Some("YE7VzlLtp-4".to_string())
        );
    }

    #[test]
    fn youtube_video_id_from_watch_with_playlist() {
        assert_eq!(
            youtube_video_id(&parse(
                "https://www.youtube.com/watch?v=YE7VzlLtp-4&list=PLabc"
            )),
            Some("YE7VzlLtp-4".to_string())
        );
    }

    #[test]
    fn youtube_video_id_from_short_url() {
        assert_eq!(
            youtube_video_id(&parse("https://youtu.be/dQw4w9WgXcQ")),
            Some("dQw4w9WgXcQ".to_string())
        );
    }

    #[test]
    fn youtube_video_id_from_embed_url() {
        assert_eq!(
            youtube_video_id(&parse("https://www.youtube.com/embed/dQw4w9WgXcQ")),
            Some("dQw4w9WgXcQ".to_string())
        );
    }

    #[test]
    fn youtube_video_id_from_shorts_url() {
        assert_eq!(
            youtube_video_id(&parse("https://www.youtube.com/shorts/abc_DEF1234")),
            Some("abc_DEF1234".to_string())
        );
    }

    #[test]
    fn youtube_video_id_rejects_invalid_chars() {
        assert_eq!(
            youtube_video_id(&parse("https://www.youtube.com/watch?v=../etc/passwd")),
            None
        );
    }

    #[test]
    fn youtube_video_id_returns_none_for_non_youtube_host() {
        assert_eq!(
            youtube_video_id(&parse("https://example.com/watch?v=abc")),
            None
        );
    }

    #[test]
    fn youtube_thumbnail_url_builds_hqdefault() {
        let thumb = youtube_thumbnail_url("YE7VzlLtp-4").unwrap();
        assert_eq!(
            thumb.as_str(),
            "https://i.ytimg.com/vi/YE7VzlLtp-4/hqdefault.jpg"
        );
    }

    #[test]
    fn youtube_thumbnail_url_rejects_invalid_id() {
        assert!(youtube_thumbnail_url("../etc/passwd").is_none());
    }
}

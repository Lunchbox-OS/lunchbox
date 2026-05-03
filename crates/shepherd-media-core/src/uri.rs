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
}

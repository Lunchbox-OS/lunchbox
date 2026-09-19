//! Turn a configured [`LibrarySource`] into a parsed
//! [`Library`](lunchbox_media_core::Library).
//!
//! This is the platform-specific half the `lunchbox-media-app` crate
//! deliberately leaves out: reading bytes (local file or HTTP) and handing them
//! to the core parser. It runs **off the UI thread** — Android throws
//! `NetworkOnMainThreadException` for any network on the main thread — so the
//! grid screen resolves on a worker and polls the result.
//!
//! Not yet handled (they need the Android JNI bridges / youtubedl-android):
//! - `content://` SAF URIs (need a `ContentResolver`).
//! - YouTube playlists (need yt-dlp).
//!
//! A `saf-toml`/`m3u` source whose locator is a plain filesystem path or
//! `file://` URI *is* resolved here, which is what makes the desktop preview
//! able to load a real local library.

use std::path::Path;

use lunchbox_media_app::LibrarySource;
use lunchbox_media_core::{Library, LibraryError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("{0}")]
    Unsupported(&'static str),

    #[error("HTTP {status} fetching `{url}`")]
    Http { url: String, status: u16 },

    #[error("network error fetching `{url}`: {msg}")]
    Network { url: String, msg: String },

    #[error("cannot read `{path}`: {msg}")]
    Io { path: String, msg: String },

    #[error(transparent)]
    Parse(#[from] LibraryError),

    #[error("YouTube: {0}")]
    YtDlp(String),
}

/// Resolve a library source into a parsed library. Blocking; call off the UI
/// thread.
pub fn resolve(source: &LibrarySource) -> Result<Library, ResolveError> {
    match source {
        LibrarySource::HttpToml { url } => {
            let body = http_get(url)?;
            parse_toml(&body, url)
        }
        LibrarySource::SafToml { uri } => {
            let path = local_path(uri).ok_or(ResolveError::Unsupported(
                "SAF content:// resolution needs the Android storage bridge (not yet wired)",
            ))?;
            let body = read_local(&path)?;
            parse_toml(&body, &path)
        }
        LibrarySource::M3u { uri } => {
            if is_http(uri) {
                let body = http_get(uri)?;
                parse_m3u(&body, uri)
            } else {
                let path = local_path(uri).ok_or(ResolveError::Unsupported(
                    "SAF content:// resolution needs the Android storage bridge (not yet wired)",
                ))?;
                let body = read_local(&path)?;
                parse_m3u(&body, &path)
            }
        }
        LibrarySource::YoutubePlaylist { url } => {
            let provider = crate::youtube::provider().ok_or(ResolveError::Unsupported(
                "YouTube isn't available on this platform (needs the Android youtubedl bridge)",
            ))?;
            let info = crate::youtube::fetch_playlist(provider.as_ref(), url)
                .map_err(ResolveError::YtDlp)?;
            Ok(lunchbox_media_core::build_library_from_entries(
                url,
                info.title,
                info.playlist_id.as_deref(),
                &info.entries,
            ))
        }
    }
}

fn parse_toml(content: &str, display_path: &str) -> Result<Library, ResolveError> {
    Ok(lunchbox_media_core::library::parse_library(
        content,
        Path::new(display_path),
    )?)
}

fn parse_m3u(content: &str, display_path: &str) -> Result<Library, ResolveError> {
    Ok(lunchbox_media_core::playlist::parse_playlist(
        content,
        Path::new(display_path),
    )?)
}

fn http_get(url: &str) -> Result<String, ResolveError> {
    let resp = ureq::get(url).call().map_err(|e| match e {
        ureq::Error::Status(code, _) => ResolveError::Http {
            url: url.to_string(),
            status: code,
        },
        other => ResolveError::Network {
            url: url.to_string(),
            msg: other.to_string(),
        },
    })?;
    resp.into_string().map_err(|e| ResolveError::Network {
        url: url.to_string(),
        msg: e.to_string(),
    })
}

fn read_local(path: &str) -> Result<String, ResolveError> {
    std::fs::read_to_string(path).map_err(|e| ResolveError::Io {
        path: path.to_string(),
        msg: e.to_string(),
    })
}

fn is_http(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Map a locator to a local filesystem path, or `None` if it isn't one we can
/// read directly (e.g. a `content://` SAF URI).
fn local_path(locator: &str) -> Option<String> {
    if let Some(rest) = locator.strip_prefix("file://") {
        Some(rest.to_string())
    } else if locator.starts_with("content://") || is_http(locator) {
        None
    } else {
        Some(locator.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(name: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("shepherd-media-resolve-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        path
    }

    const TOML_LIB: &str = r#"
        schema_version = 1
        library_id = "demo"
        title = "Demo"

        [[items]]
        id = "a"
        title = "A"
        kind = "video"

        [[items.sources]]
        platforms = ["*"]
        uri = "file:///srv/a.mp4"
    "#;

    #[test]
    fn resolves_local_toml() {
        let path = temp_file("lib.toml", TOML_LIB);
        let src = LibrarySource::SafToml {
            uri: path.to_string_lossy().into_owned(),
        };
        let lib = resolve(&src).unwrap();
        assert_eq!(lib.library_id, "demo");
        assert_eq!(lib.items.len(), 1);
    }

    #[test]
    fn resolves_file_uri() {
        let path = temp_file("lib2.toml", TOML_LIB);
        let src = LibrarySource::SafToml {
            uri: format!("file://{}", path.to_string_lossy()),
        };
        assert!(resolve(&src).is_ok());
    }

    #[test]
    fn resolves_local_m3u() {
        let body = "#EXTM3U\n#EXTINF:10,Clip\nfile:///srv/clip.mp4\n";
        let path = temp_file("list.m3u", body);
        let src = LibrarySource::M3u {
            uri: path.to_string_lossy().into_owned(),
        };
        let lib = resolve(&src).unwrap();
        assert_eq!(lib.items.len(), 1);
    }

    #[test]
    fn content_uri_is_unsupported() {
        let src = LibrarySource::SafToml {
            uri: "content://com.android.providers/movies.toml".to_string(),
        };
        assert!(matches!(resolve(&src), Err(ResolveError::Unsupported(_))));
    }

    /// Only a platform with no yt-dlp provider reports YouTube as unsupported,
    /// so this is gated to one. Compiled for Android, `youtube::provider()`
    /// returns the JNI binding and `resolve` really runs it — which panics in
    /// `ndk_context` under a bare test binary, since that has no JVM and no
    /// Activity. The app itself always does (android-activity initializes the
    /// context at startup), so there is nothing to guard against there.
    #[cfg(not(target_os = "android"))]
    #[test]
    fn youtube_is_unsupported_without_a_provider() {
        let src = LibrarySource::YoutubePlaylist {
            url: "https://www.youtube.com/playlist?list=PL".to_string(),
        };
        assert!(matches!(resolve(&src), Err(ResolveError::Unsupported(_))));
    }

    #[test]
    fn missing_local_file_is_io_error() {
        let src = LibrarySource::SafToml {
            uri: "/nonexistent/path/library.toml".to_string(),
        };
        assert!(matches!(resolve(&src), Err(ResolveError::Io { .. })));
    }

    #[test]
    fn malformed_toml_is_parse_error() {
        let path = temp_file("bad.toml", "schema_version = 1\nbogus = true\n");
        let src = LibrarySource::SafToml {
            uri: path.to_string_lossy().into_owned(),
        };
        assert!(matches!(resolve(&src), Err(ResolveError::Parse(_))));
    }
}

//! Caching and quality policy types for the Android settings model.
//!
//! - [`Quality`] is the shared `--quality` preset enum — the Linux binary reuses
//!   it via this crate's `clap` feature — plus the `ytdl_format` selector.
//! - [`CacheMode`] mirrors the two `VideoCache` strategies (`queue_all` /
//!   `queue_after_play`) plus an explicit `off`.
//! - [`PosterPolicy`] gates poster fetching on metered/Wi-Fi connectivity, a
//!   concern that only arises on a battery- and data-constrained device.

use serde::{Deserialize, Serialize};

/// Maximum video quality for playback and background downloads.
///
/// Serializes to the same spellings the CLI accepts (`best`, `1080p`, `720p`,
/// `480p`). With the `clap` feature this is also the Linux binary's
/// `--quality` value type, so the two front-ends share one definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    /// No height restriction — download the best available quality.
    Best,
    /// Up to 1080p (default).
    #[default]
    #[serde(rename = "1080p")]
    #[cfg_attr(feature = "clap", value(name = "1080p"))]
    Q1080,
    /// Up to 720p.
    #[serde(rename = "720p")]
    #[cfg_attr(feature = "clap", value(name = "720p"))]
    Q720,
    /// Up to 480p.
    #[serde(rename = "480p")]
    #[cfg_attr(feature = "clap", value(name = "480p"))]
    Q480,
}

impl Quality {
    /// Human-readable label for a settings UI (e.g. the Android quality picker).
    /// Distinct from the serde spelling only for `Best` ("Best" vs "best").
    pub fn label(self) -> &'static str {
        match self {
            Quality::Best => "Best",
            Quality::Q1080 => "1080p",
            Quality::Q720 => "720p",
            Quality::Q480 => "480p",
        }
    }

    /// Returns the yt-dlp `--format` / mpv `ytdl-format` string for this preset.
    ///
    /// This is the single definition for both front-ends (the Linux binary
    /// reuses it via the `clap` feature).
    ///
    /// Every preset asks for **H.264 first** (`vcodec^=avc1`). yt-dlp ranks
    /// codecs `av01 > vp9 > avc1` at equal resolution, so a plain
    /// `bestvideo[height<=?1080]` selects VP9 or AV1 for most YouTube uploads —
    /// and the fixed-function decoders in the hardware this project targets
    /// mostly don't cover those. On an Intel HD 4000 that difference is a
    /// software VP9 decode at ~61% of a CPU core against a hardware H.264
    /// decode at ~13% for the same video (issue #115). The `bv*+ba` and `b`
    /// tails fall back to any codec, then to a muxed format, so an upload with
    /// no H.264 rendition still plays.
    ///
    /// The same reasoning already governs the Android app's stream selector
    /// (`lunchbox_media_android::youtube::stream_format`), which resolves
    /// streams itself instead of going through mpv's `ytdl_hook`.
    pub fn ytdl_format(self) -> &'static str {
        match self {
            Quality::Best => "bv*[vcodec^=avc1]+ba/bv*+ba/b",
            Quality::Q1080 => {
                "bv*[vcodec^=avc1][height<=?1080]+ba/bv*[height<=?1080]+ba/b[height<=?1080]/b"
            }
            Quality::Q720 => {
                "bv*[vcodec^=avc1][height<=?720]+ba/bv*[height<=?720]+ba/b[height<=?720]/b"
            }
            Quality::Q480 => {
                "bv*[vcodec^=avc1][height<=?480]+ba/bv*[height<=?480]+ba/b[height<=?480]/b"
            }
        }
    }
}

/// How aggressively to cache remote video sources to local storage.
///
/// Mirrors the strategies implemented in `lunchbox-media`'s `VideoCache`:
/// `QueueAfterPlay` is "Option B" (cache an item after it finishes so the next
/// play is local) and `QueueAll` is "Option A" (prefetch every remote item at
/// browse launch, without displacing existing cached content).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CacheMode {
    /// Never download; always stream remote sources. The conservative default.
    #[default]
    Off,
    /// Cache an item after it finishes playing (EOF), evicting LRU files to stay
    /// within the cache cap.
    QueueAfterPlay,
    /// Prefetch every remote item in the library at browse launch, skipping when
    /// the cache is already at capacity.
    QueueAll,
}

/// When the app may fetch remote posters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PosterPolicy {
    /// Fetch posters on any connection (default).
    #[default]
    Always,
    /// Fetch posters only on an unmetered (Wi-Fi) connection.
    WifiOnly,
    /// Never fetch remote posters; show only what is already cached or local.
    Never,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_serde_spellings() {
        for (q, s) in [
            (Quality::Best, "\"best\""),
            (Quality::Q1080, "\"1080p\""),
            (Quality::Q720, "\"720p\""),
            (Quality::Q480, "\"480p\""),
        ] {
            assert_eq!(toml_value(&q), s);
            assert_eq!(from_toml_value::<Quality>(s), q);
        }
    }

    #[test]
    fn cache_mode_serde_spellings() {
        assert_eq!(toml_value(&CacheMode::Off), "\"off\"");
        assert_eq!(
            toml_value(&CacheMode::QueueAfterPlay),
            "\"queue-after-play\""
        );
        assert_eq!(toml_value(&CacheMode::QueueAll), "\"queue-all\"");
    }

    #[test]
    fn poster_policy_serde_spellings() {
        assert_eq!(toml_value(&PosterPolicy::Always), "\"always\"");
        assert_eq!(toml_value(&PosterPolicy::WifiOnly), "\"wifi-only\"");
        assert_eq!(toml_value(&PosterPolicy::Never), "\"never\"");
    }

    #[test]
    fn defaults_are_conservative() {
        assert_eq!(Quality::default(), Quality::Q1080);
        assert_eq!(CacheMode::default(), CacheMode::Off);
        assert_eq!(PosterPolicy::default(), PosterPolicy::Always);
    }

    #[test]
    fn quality_ytdl_format_strings() {
        // This crate is now the single source of these selectors for both the
        // Linux binary and the Android app; pin them so a change is deliberate.
        assert_eq!(Quality::Best.ytdl_format(), "bv*[vcodec^=avc1]+ba/bv*+ba/b");
        assert_eq!(
            Quality::Q1080.ytdl_format(),
            "bv*[vcodec^=avc1][height<=?1080]+ba/bv*[height<=?1080]+ba/b[height<=?1080]/b"
        );
    }

    #[test]
    fn every_quality_prefers_h264_then_falls_back() {
        // The H.264 preference is what keeps playback on the hardware decoder
        // (issue #115), and the fallbacks are what keep an upload with no H.264
        // rendition playable. Neither half is optional.
        for q in [Quality::Best, Quality::Q1080, Quality::Q720, Quality::Q480] {
            let fmt = q.ytdl_format();
            assert!(
                fmt.starts_with("bv*[vcodec^=avc1]"),
                "{q:?} must ask for H.264 first, got {fmt}"
            );
            assert!(
                fmt.split('/').count() >= 3,
                "{q:?} must fall back to any codec and then to a muxed format, got {fmt}"
            );
            assert!(
                fmt.ends_with("/b"),
                "{q:?} must end with an unconstrained muxed fallback, got {fmt}"
            );
        }
    }

    #[test]
    fn height_capped_qualities_constrain_every_selector_but_the_last_resort() {
        // A cap that only lands on the first alternative would silently pull a
        // 4K stream the moment an upload has no H.264 rendition.
        for (q, cap) in [
            (Quality::Q1080, "1080"),
            (Quality::Q720, "720"),
            (Quality::Q480, "480"),
        ] {
            let alternatives: Vec<&str> = q.ytdl_format().split('/').collect();
            let (last, capped) = alternatives.split_last().expect("non-empty selector");
            assert_eq!(
                *last, "b",
                "{q:?} should end with the last-resort muxed `b`"
            );
            for alt in capped {
                assert!(
                    alt.contains(&format!("height<=?{cap}")),
                    "{q:?} selector `{alt}` is missing the {cap}p cap"
                );
            }
        }
    }

    // Tiny round-trip helpers that exercise the real (TOML) format by wrapping
    // the value in a one-field table and extracting the rendered scalar.
    fn toml_value<T: Serialize>(v: &T) -> String {
        #[derive(Serialize)]
        struct W<'a, U: Serialize> {
            x: &'a U,
        }
        let s = toml::to_string(&W { x: v }).unwrap();
        s.trim().strip_prefix("x = ").unwrap().to_string()
    }

    fn from_toml_value<T: for<'de> Deserialize<'de>>(quoted: &str) -> T {
        #[derive(Deserialize)]
        struct W<U> {
            x: U,
        }
        let w: W<T> = toml::from_str(&format!("x = {quoted}")).unwrap();
        w.x
    }
}

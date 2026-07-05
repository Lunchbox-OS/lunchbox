//! Caching and quality policy types.
//!
//! These mirror options that already exist in the Linux binary so the Android
//! settings model expresses the same knobs:
//!
//! - [`Quality`] mirrors `shepherd-media`'s `--quality` presets (and the
//!   `ytdl_format` selector they map to).
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
    pub fn ytdl_format(self) -> &'static str {
        match self {
            Quality::Best => "bestvideo+bestaudio/best",
            Quality::Q1080 => "bestvideo[height<=?1080]+bestaudio/best[height<=?1080]/best",
            Quality::Q720 => "bestvideo[height<=?720]+bestaudio/best[height<=?720]/best",
            Quality::Q480 => "bestvideo[height<=?480]+bestaudio/best[height<=?480]/best",
        }
    }
}

/// How aggressively to cache remote video sources to local storage.
///
/// Mirrors the strategies implemented in `shepherd-media`'s `VideoCache`:
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
        assert_eq!(Quality::Best.ytdl_format(), "bestvideo+bestaudio/best");
        assert_eq!(
            Quality::Q1080.ytdl_format(),
            "bestvideo[height<=?1080]+bestaudio/best[height<=?1080]/best"
        );
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

//! The background download worker: one thread, a queue of requests, yt-dlp or
//! a plain HTTP fetch per item, then eviction back to the cap.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

use shepherd_media_app::lru::{Score, ScoreWeights};
use tracing::{debug, warn};

use crate::lock::DownloadLock;
use crate::store::{
    CacheState, cache_state, cache_total, clear_failed, evict_for, mark_failed, mark_played,
    mark_seen, prospective_score, remove_cached_item, retry_blocked, write_done_sentinel,
};

/// How long a failed speculative download is left alone before it is tried
/// again.
///
/// Longer than the hourly sweep on purpose: the failures worth pacing are the
/// ones that do not fix themselves within an hour — a video made private, a
/// format that no longer exists, a client setting YouTube has started refusing.
/// A download the user earned by watching is never held back by this.
pub const RETRY_COOLDOWN: Duration = Duration::from_secs(6 * 60 * 60);

/// Default wait between downloads.
///
/// Prefetch is speculative work on somebody else's servers, and a first sweep
/// over a large library is a hundred back-to-back yt-dlp invocations from one
/// address. Nothing observed has been attributed to that rate, but there is no
/// reason to be in a hurry: the content is not wanted yet, and the sweep has an
/// hour before the next one. Skipped items cost nothing — the wait is only
/// after an attempt that actually reached the network.
pub const DEFAULT_DOWNLOAD_INTERVAL: Duration = Duration::from_secs(5);

/// How a queued URL is fetched.
#[derive(Debug, Clone, Copy)]
pub enum DownloadKind {
    YouTube,
    Http,
}

impl DownloadKind {
    /// The format selector recorded in (and compared against) the done
    /// sentinel. A direct HTTP download picks no format, so it records none.
    pub fn selector<'a>(&self, ytdl_format: &'a str) -> &'a str {
        match self {
            DownloadKind::YouTube => ytdl_format,
            DownloadKind::Http => "",
        }
    }
}

pub struct DownloadRequest {
    /// Content key: names the files on disk (see [`crate::key`]).
    pub key: String,
    /// Interest key: names the video's `.played` and `.seen` markers. Recorded
    /// in the sentinel so eviction can find them from a directory walk.
    pub interest_key: String,
    /// Human-readable name for logs — a library item id. Never a filename.
    pub label: String,
    pub url: String,
    pub kind: DownloadKind,
    /// Where the item sat in its library when it was queued, for a prefetch.
    /// `None` for a download earned by watching, which is scored as a play and
    /// never needs it.
    pub ordinal: Option<u32>,
    /// Whether the user earned this download by watching the previous video
    /// through to the end. An earned download is scored as a play, so it may
    /// displace almost anything; a speculative one is scored as the guess it is
    /// and only spends what it outranks.
    pub earned: bool,
}

pub fn download_worker(
    cache_dir: PathBuf,
    rx: mpsc::Receiver<DownloadRequest>,
    max_bytes: u64,
    ytdl_format: String,
    weights: ScoreWeights,
    download_interval: Duration,
) {
    for req in rx {
        // Record the sighting even for an item this pass will not download: an
        // item the cache had no room for still has to be correctly aged when
        // room appears, and the marker is write-once so this is free after the
        // first time.
        mark_seen(&cache_dir, &req.interest_key);

        if cache_state(&cache_dir, &req.key) == CacheState::Present {
            debug!("video already cached, skipping: {}", req.label);
            continue;
        }

        // A speculative download that failed recently is left alone. The user
        // is not waiting on it, and retrying every sweep turns one broken item
        // into an hourly warning forever.
        if !req.earned && retry_blocked(&cache_dir, &req.key, RETRY_COOLDOWN) {
            debug!("{} failed recently; not retrying yet", req.label);
            continue;
        }

        // What this download is worth, and therefore what it is allowed to
        // spend. Computed once, before the download, so the pre-flight check
        // and the trim that follows agree.
        let incoming = if req.earned {
            Score::earned_at(SystemTime::now(), weights)
        } else {
            prospective_score(
                &cache_dir,
                &req.interest_key,
                req.ordinal,
                SystemTime::now(),
                weights,
            )
        };

        if !req.earned {
            // Is there anything here cheap enough to make room with? If not,
            // the guess is dropped rather than made to cost the child a file it
            // does not outrank.
            if cache_total(&cache_dir) >= max_bytes {
                // Strictly below the cap: at exactly the cap there is room for
                // nothing, so evicting "to the cap" would be a no-op and the
                // cache would stay frozen. The post-download trim below puts it
                // back within bounds once the real size is known.
                evict_for(&cache_dir, max_bytes.saturating_sub(1), incoming, weights);
            }
            if cache_total(&cache_dir) >= max_bytes {
                debug!(
                    "cache holds nothing worth less than {}; skipping prefetch",
                    req.label
                );
                continue;
            }
        }

        // Claim the key. Another process may be fetching it already — a
        // prefetching shepherdd and a playing shepherd-media share this
        // directory — in which case skip rather than corrupt its `.part` file.
        let Some(_lock) = DownloadLock::try_acquire(&cache_dir, &req.key) else {
            debug!("another process is downloading {}; skipping", req.label);
            continue;
        };

        // Re-check under the lock: the holder we lost to a moment ago may have
        // just committed exactly this file.
        if cache_state(&cache_dir, &req.key) == CacheState::Present {
            debug!("{} was cached while we waited to claim it", req.label);
            continue;
        }
        // Clear anything half-written under this key: a previous attempt may
        // have left a file on a different extension, and two files for one key
        // make playback ambiguous.
        remove_cached_item(&cache_dir, &req.key);

        debug!("downloading video: {}", req.label);
        let result = match req.kind {
            DownloadKind::YouTube => download_youtube(
                &cache_dir,
                &req.key,
                &req.interest_key,
                &req.url,
                &ytdl_format,
                req.ordinal,
            ),
            DownloadKind::Http => download_http(
                &cache_dir,
                &req.key,
                &req.interest_key,
                &req.url,
                req.ordinal,
            ),
        };

        match result {
            Ok(()) => {
                debug!("cached video: {}", req.label);
                clear_failed(&cache_dir, &req.key);
                // An earned download exists *because* the child watched this
                // video, so record that before trimming — otherwise the file
                // arrives scored as a guess and is the first thing the trim
                // below throws away, i.e. it evicts itself.
                if req.earned {
                    mark_played(&cache_dir, &req.interest_key);
                }
                // Trim back to the cap, spending exactly what the pre-flight
                // said this download was worth.
                evict_for(&cache_dir, max_bytes, incoming, weights);
            }
            Err(e) => {
                warn!("failed to cache video {}: {e}", req.label);
                mark_failed(&cache_dir, &req.key);
                // Whatever yt-dlp left behind is not a usable video and has no
                // sentinel, so nothing would serve it — but it still occupies
                // the disk until something else needs the space.
                remove_cached_item(&cache_dir, &req.key);
            }
        }

        // Pace the next attempt. After the work, so a queue of cache hits still
        // drains immediately.
        if !download_interval.is_zero() {
            std::thread::sleep(download_interval);
        }
    }
}

fn download_youtube(
    cache_dir: &Path,
    key: &str,
    interest_key: &str,
    url: &str,
    ytdl_format: &str,
    ordinal: Option<u32>,
) -> Result<(), String> {
    let output_template = cache_dir.join(format!("{key}.%(ext)s"));
    // In a cgroup of its own where one can be had (issue #144): this parses
    // whatever the remote host sends back, and a direct child of shepherdd is
    // inside the management socket's allow-list. See `crate::subprocess`.
    let output = crate::subprocess::ytdlp_command("ytdlp-download")
        .args([
            "--quiet",
            "--no-warnings",
            // Match playback's player clients so DRM-protected uploads download
            // their progressive itag-18 fallback instead of failing.
            "--extractor-args",
            shepherd_media_core::YOUTUBE_EXTRACTOR_ARGS,
            "--format",
            ytdl_format,
            "--output",
        ])
        .arg(&output_template)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        // Captured, not discarded. Everything that goes wrong here goes wrong
        // *inside* yt-dlp — a 403 on the media URL, a format that matches
        // nothing, an age gate — and the exit status is 1 for all of it. Without
        // this the log can only say "exited with 1", which is what turned one
        // broken client setting into an afternoon of bisecting by hand.
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to spawn yt-dlp: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "yt-dlp exited with {} for {url}{}",
            output.status,
            format_stderr(&output.stderr)
        ));
    }
    write_done_sentinel(cache_dir, key, interest_key, ytdl_format, ordinal)
}

/// The tail of a failed command's stderr, for appending to an error message.
///
/// Bounded: yt-dlp can emit a great deal on failure, and a cache warning should
/// not put a screenful into the journal for every item of a library that has
/// stopped working. The last lines are the ones that say why.
fn format_stderr(stderr: &[u8]) -> String {
    const MAX_LINES: usize = 3;
    const MAX_CHARS: usize = 400;

    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    let tail = &lines[lines.len().saturating_sub(MAX_LINES)..];
    let mut joined = tail.join("; ");
    if joined.chars().count() > MAX_CHARS {
        joined = joined.chars().take(MAX_CHARS).collect::<String>() + "…";
    }
    format!(": {joined}")
}

fn download_http(
    cache_dir: &Path,
    key: &str,
    interest_key: &str,
    url: &str,
    ordinal: Option<u32>,
) -> Result<(), String> {
    let ext = url
        .rsplit('.')
        .next()
        .filter(|s| s.len() <= 5 && s.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or("bin");

    let part_path = cache_dir.join(format!("{key}.part"));
    let final_path = cache_dir.join(format!("{key}.{ext}"));

    let resp = ureq::get(url)
        .call()
        .map_err(|e| format!("HTTP request failed for {url}: {e}"))?;

    let mut reader = resp.into_reader();
    let mut file = std::fs::File::create(&part_path)
        .map_err(|e| format!("failed to create part file: {e}"))?;

    std::io::copy(&mut reader, &mut file).map_err(|e| format!("failed to write download: {e}"))?;

    std::fs::rename(&part_path, &final_path)
        .map_err(|e| format!("failed to rename part file: {e}"))?;

    // No format selector is involved in a direct download.
    write_done_sentinel(cache_dir, key, interest_key, "", ordinal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_stderr_adds_nothing_to_the_message() {
        assert_eq!(format_stderr(b""), "");
        assert_eq!(format_stderr(b"\n  \n"), "");
    }

    #[test]
    fn the_last_lines_are_the_ones_that_say_why() {
        // yt-dlp puts the diagnosis at the end, after whatever progress and
        // extractor chatter preceded it.
        let out = format_stderr(b"noise\nmore noise\na\nb\nERROR: HTTP Error 403: Forbidden\n");
        assert!(out.starts_with(": "));
        assert!(out.contains("ERROR: HTTP Error 403: Forbidden"));
        assert!(!out.contains("noise"), "the tail is bounded: {out}");
    }

    #[test]
    fn a_flood_is_truncated() {
        // A library that has entirely stopped working must not put a screenful
        // into the journal per item.
        let flood = "x".repeat(10_000);
        let out = format_stderr(flood.as_bytes());
        assert!(out.chars().count() < 500, "{} chars", out.chars().count());
        assert!(out.ends_with('…'));
    }

    #[test]
    fn utf8_damage_does_not_panic() {
        assert!(format_stderr(&[0xff, 0xfe, b'h', b'i']).contains("hi"));
    }
}

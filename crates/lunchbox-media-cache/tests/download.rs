//! End-to-end exercise of the download worker against a loopback HTTP server.
//!
//! Unit tests cover the cache's on-disk classification by writing files
//! directly; this drives the real path — queue, claim, fetch, commit, look up —
//! so the pieces are checked against each other rather than against a fixture.
//!
//! Loopback only: no external network, nothing to be flaky in CI.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use filetime::FileTime;

use lunchbox_media_cache::{
    CacheWeights, VideoCache, VideoCacheConfig, content_key, interest_key, source_url,
};
use lunchbox_media_core::{ClassifiedUri, PlayerHint, Source};

const BODY: &[u8] = b"not really a video, but it is bytes";

/// Serve `BODY` to `count` requests on an ephemeral loopback port, then stop.
/// Returns the base URL.
fn serve(count: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for _ in 0..count {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            // Read just the request head; we serve the same body regardless.
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = write_response(&mut stream);
        }
    });
    url
}

/// Serve `count` failures on an ephemeral loopback port, then stop.
fn serve_failing(count: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for _ in 0..count {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = write!(
                stream,
                "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
        }
    });
    url
}

fn write_response(stream: &mut TcpStream) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        BODY.len()
    )?;
    stream.write_all(BODY)?;
    stream.flush()
}

fn http_source(url: &str) -> Source {
    Source {
        platforms: Vec::new(),
        uri: ClassifiedUri::DirectHttp(url.parse().expect("valid url")),
        player_hint: Some(PlayerHint::Mpv),
    }
}

fn cache_in(dir: &Path, max_bytes: u64) -> Arc<VideoCache> {
    VideoCache::with_config(VideoCacheConfig {
        cache_dir: dir.to_path_buf(),
        max_bytes,
        ytdl_format: "test-selector".into(),
        weights: CacheWeights::default(),
        // No pacing: these tests poll for the worker's output on a deadline.
        download_interval: Duration::ZERO,
    })
    .expect("cache constructs")
}

/// Backdate a source's bookkeeping so it reads as genuinely old rather than as
/// something this test wrote a moment ago. `.seen` is what ages an unwatched
/// file; the video's own mtime is a floor on it, so both have to move.
fn backdate(dir: &Path, source: &Source, secs_ago: i64, cache: &VideoCache) {
    let when = FileTime::from_unix_time(FileTime::now().unix_seconds() - secs_ago, 0);
    let ikey = interest_key(&source_url(source).unwrap());
    for name in [format!("{ikey}.seen"), format!("{ikey}.played")] {
        let path = dir.join(name);
        if path.exists() {
            filetime::set_file_mtime(&path, when).unwrap();
        }
    }
    if let Some(path) = cache.cached_path(source) {
        filetime::set_file_mtime(path, when).unwrap();
    }
}

const DAY: i64 = 24 * 60 * 60;

/// Wait for `f` to hold, up to 10s. The worker runs on its own thread, so
/// every assertion about its output has to be a poll rather than a read.
fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn a_prefetched_video_lands_under_its_url_hash_and_is_then_a_cache_hit() {
    let dir = tempfile::tempdir().unwrap();
    let base = serve(1);
    let url = format!("{base}/clip.mp4");
    let source = http_source(&url);

    let cache = cache_in(dir.path(), 1024 * 1024);
    assert!(
        cache.cached_path(&source).is_none(),
        "nothing is cached before the prefetch"
    );

    cache.queue_prefetch("clip", &source, 0);
    wait_for("the download to commit", || {
        cache.cached_path(&source).is_some()
    });

    let path = cache.cached_path(&source).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), BODY);

    // Named by URL hash, not by the label — that is the whole point of the
    // keying change, and the label never reaches the filesystem. A direct-HTTP
    // source carries no format selector, so its content key uses an empty one.
    let key = content_key(&url, "");
    assert_eq!(path.file_stem().unwrap().to_str().unwrap(), key);
    assert_ne!(path.file_stem().unwrap().to_str().unwrap(), "clip");
    assert!(dir.path().join(format!("{key}.done")).exists());
    // The extension is taken from the URL, so the file is playable by name.
    assert_eq!(path.extension().unwrap(), "mp4");
    // No partial file survives a committed download.
    assert!(!dir.path().join(format!("{key}.part")).exists());
}

#[test]
fn a_second_queue_of_a_cached_item_does_not_refetch_it() {
    let dir = tempfile::tempdir().unwrap();
    // Exactly one request will be served. A re-fetch would hang, then fail.
    let base = serve(1);
    let url = format!("{base}/clip.mp4");
    let source = http_source(&url);

    let cache = cache_in(dir.path(), 1024 * 1024);
    cache.queue_prefetch("clip", &source, 0);
    wait_for("the first download", || {
        cache.cached_path(&source).is_some()
    });

    let path = cache.cached_path(&source).unwrap();
    let first = std::fs::metadata(&path).unwrap().modified().unwrap();

    cache.queue_prefetch("clip", &source, 0);
    std::thread::sleep(Duration::from_millis(200));

    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        first,
        "a cache hit must not be re-downloaded"
    );
    assert_eq!(std::fs::read(&path).unwrap(), BODY);
}

/// Two libraries that both call their item `intro` must not share a file —
/// the collision the URL keying exists to prevent.
#[test]
fn two_libraries_with_the_same_item_id_get_two_files() {
    let dir = tempfile::tempdir().unwrap();
    let base = serve(2);
    let one = http_source(&format!("{base}/one/intro.mp4"));
    let two = http_source(&format!("{base}/two/intro.mp4"));

    let cache = cache_in(dir.path(), 1024 * 1024);
    cache.queue_prefetch("intro", &one, 0);
    cache.queue_prefetch("intro", &two, 1);

    wait_for("both downloads", || {
        cache.cached_path(&one).is_some() && cache.cached_path(&two).is_some()
    });
    assert_ne!(
        cache.cached_path(&one).unwrap(),
        cache.cached_path(&two).unwrap(),
        "the two items must occupy separate files"
    );
}

/// An item the parent added recently may recycle space held by a guess that has
/// been sitting there since long before it — this is what stops a full cache
/// going inert on content nobody has touched.
#[test]
fn a_new_arrival_recycles_space_held_by_a_stale_guess() {
    let dir = tempfile::tempdir().unwrap();
    let base = serve(2);
    let stale = http_source(&format!("{base}/stale.mp4"));
    let added = http_source(&format!("{base}/added.mp4"));

    // Room for one download at a time.
    let cache = cache_in(dir.path(), BODY.len() as u64);
    cache.queue_prefetch("stale", &stale, 0);
    wait_for("the first download", || cache.cached_path(&stale).is_some());
    backdate(dir.path(), &stale, 60 * DAY, &cache);

    // Deliberately further down the library than the file it displaces:
    // position breaks ties, it does not outweigh two months of age.
    cache.queue_prefetch("added", &added, 40);
    wait_for("the new arrival to be downloaded", || {
        cache.cached_path(&added).is_some()
    });
    wait_for("the stale guess to be recycled", || {
        cache.cached_path(&stale).is_none()
    });
}

/// Within one sweep, though, a guess must not displace a guess nearer the head
/// of the same library: that file is the one a browsing child reaches first,
/// and evicting it would have the next pass immediately re-download it.
#[test]
fn a_prefetch_will_not_displace_a_guess_nearer_the_head_of_its_library() {
    let dir = tempfile::tempdir().unwrap();
    let base = serve(2);
    let head = http_source(&format!("{base}/head.mp4"));
    let tail = http_source(&format!("{base}/tail.mp4"));

    let cache = cache_in(dir.path(), BODY.len() as u64);
    cache.queue_prefetch("head", &head, 0);
    wait_for("the first download", || cache.cached_path(&head).is_some());

    cache.queue_prefetch("tail", &tail, 12);
    std::thread::sleep(Duration::from_millis(300));

    assert!(cache.cached_path(&head).is_some());
    assert!(
        cache.cached_path(&tail).is_none(),
        "the tail of the list must be dropped, not swapped for the head"
    );
}

/// A guess must never displace something the child watched recently. It is
/// dropped rather than made to cost them a film they chose.
#[test]
fn a_prefetch_will_not_displace_a_recently_watched_video() {
    let dir = tempfile::tempdir().unwrap();
    let base = serve(2);
    let watched = http_source(&format!("{base}/watched.mp4"));
    let guess = http_source(&format!("{base}/guess.mp4"));

    let cache = cache_in(dir.path(), BODY.len() as u64);
    cache.queue_prefetch("watched", &watched, 0);
    wait_for("the first download", || {
        cache.cached_path(&watched).is_some()
    });
    cache.mark_played(&watched);

    cache.queue_prefetch("guess", &guess, 1);
    std::thread::sleep(Duration::from_millis(300));

    assert!(
        cache.cached_path(&watched).is_some(),
        "a watched video must survive a prefetch that wants its space"
    );
    assert!(
        cache.cached_path(&guess).is_none(),
        "the guess must be dropped rather than made to cost the watched file"
    );
}

/// ...but that protection expires. Once a play is old enough to have lost its
/// grace the file competes on age like anything else, which is what keeps a
/// cache full of watched content from freezing forever.
#[test]
fn a_prefetch_may_displace_a_video_watched_long_ago() {
    let dir = tempfile::tempdir().unwrap();
    let base = serve(2);
    let watched = http_source(&format!("{base}/watched.mp4"));
    let guess = http_source(&format!("{base}/guess.mp4"));

    let cache = cache_in(dir.path(), BODY.len() as u64);
    cache.queue_prefetch("watched", &watched, 0);
    wait_for("the first download", || {
        cache.cached_path(&watched).is_some()
    });
    cache.mark_played(&watched);
    // Well past the default 30-day grace.
    backdate(dir.path(), &watched, 90 * DAY, &cache);

    cache.queue_prefetch("guess", &guess, 1);
    wait_for("the guess to be downloaded", || {
        cache.cached_path(&guess).is_some()
    });
    wait_for("the long-unwatched film to be recycled", || {
        cache.cached_path(&watched).is_none()
    });
}

/// The after-play path is the one allowed to spend watched space: the user just
/// watched this item, so it earns its place at the expense of the
/// least-recently-watched file.
#[test]
fn an_after_play_download_evicts_to_make_room() {
    let dir = tempfile::tempdir().unwrap();
    let base = serve(2);
    let old = http_source(&format!("{base}/old.mp4"));
    let watched = http_source(&format!("{base}/watched.mp4"));

    let cache = cache_in(dir.path(), BODY.len() as u64);
    cache.queue_prefetch("old", &old, 0);
    wait_for("the first download", || cache.cached_path(&old).is_some());
    cache.mark_played(&old);
    // Watched, but a while back: an earned download outranks everything except
    // an equally recent play, and this makes "equally recent" unambiguous
    // rather than a matter of filesystem timestamp granularity.
    backdate(dir.path(), &old, 3 * DAY, &cache);

    cache.queue_after_play("watched", &watched);
    wait_for("the after-play download", || {
        cache.cached_path(&watched).is_some()
    });
    wait_for("the eviction that follows it", || {
        cache.cached_path(&old).is_none()
    });
}

/// A speculative download that fails is recorded, so the next sweep does not
/// retry it at full speed. Before this, a library whose videos had all become
/// unavailable produced an hourly burst of doomed fetches and one warning per
/// item, forever.
#[test]
fn a_failed_prefetch_is_not_retried_immediately() {
    let dir = tempfile::tempdir().unwrap();
    // Exactly one request is served. A retry would hang on accept, then fail —
    // so if the cooldown does not hold, the second `cached_path` still shows
    // nothing and the marker's mtime is what proves no second attempt happened.
    let base = serve_failing(1);
    let source = http_source(&format!("{base}/clip.mp4"));

    let cache = cache_in(dir.path(), 1024 * 1024);
    cache.queue_prefetch("clip", &source, 0);

    let key = content_key(&source_url(&source).unwrap(), "");
    let marker = dir.path().join(format!("{key}.failed"));
    wait_for("the failure to be recorded", || marker.exists());
    let first = std::fs::metadata(&marker).unwrap().modified().unwrap();

    cache.queue_prefetch("clip", &source, 0);
    std::thread::sleep(Duration::from_millis(300));

    assert_eq!(
        std::fs::metadata(&marker).unwrap().modified().unwrap(),
        first,
        "a recent failure must suppress the retry, not re-record it"
    );
    assert!(cache.cached_path(&source).is_none());
}

/// ...but a download the user earned by watching is always attempted. They are
/// waiting on it, and a stale marker must not be why they get nothing.
#[test]
fn an_earned_download_ignores_the_failure_cooldown() {
    let dir = tempfile::tempdir().unwrap();
    // Two requests: the failing prefetch, then the earned retry that succeeds.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let mut served = 0;
        while let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            if served == 0 {
                let _ = write!(
                    stream,
                    "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
            } else {
                let _ = write_response(&mut stream);
            }
            served += 1;
            if served >= 2 {
                return;
            }
        }
    });
    let source = http_source(&format!("{base}/clip.mp4"));

    let cache = cache_in(dir.path(), 1024 * 1024);
    cache.queue_prefetch("clip", &source, 0);

    let key = content_key(&source_url(&source).unwrap(), "");
    wait_for("the failure to be recorded", || {
        dir.path().join(format!("{key}.failed")).exists()
    });

    cache.queue_after_play("clip", &source);
    wait_for("the earned download to succeed", || {
        cache.cached_path(&source).is_some()
    });
    assert!(
        !dir.path().join(format!("{key}.failed")).exists(),
        "a success must forget the failure"
    );
}

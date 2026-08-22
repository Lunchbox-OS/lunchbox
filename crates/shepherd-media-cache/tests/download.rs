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

use shepherd_media_cache::{VideoCache, VideoCacheConfig, content_key};
use shepherd_media_core::{ClassifiedUri, PlayerHint, Source};

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
    })
    .expect("cache constructs")
}

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

    cache.queue_prefetch("clip", &source);
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
    cache.queue_prefetch("clip", &source);
    wait_for("the first download", || {
        cache.cached_path(&source).is_some()
    });

    let path = cache.cached_path(&source).unwrap();
    let first = std::fs::metadata(&path).unwrap().modified().unwrap();

    cache.queue_prefetch("clip", &source);
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
    cache.queue_prefetch("intro", &one);
    cache.queue_prefetch("intro", &two);

    wait_for("both downloads", || {
        cache.cached_path(&one).is_some() && cache.cached_path(&two).is_some()
    });
    assert_ne!(
        cache.cached_path(&one).unwrap(),
        cache.cached_path(&two).unwrap(),
        "the two items must occupy separate files"
    );
}

/// A speculative prefetch may recycle space held by *other* guesses — this is
/// what stops the cache going inert once it fills with prefetched content.
#[test]
fn a_prefetch_at_capacity_recycles_unwatched_space() {
    let dir = tempfile::tempdir().unwrap();
    let base = serve(2);
    let first = http_source(&format!("{base}/first.mp4"));
    let second = http_source(&format!("{base}/second.mp4"));

    // Room for one download at a time.
    let cache = cache_in(dir.path(), BODY.len() as u64);
    cache.queue_prefetch("first", &first);
    wait_for("the first download", || cache.cached_path(&first).is_some());

    cache.queue_prefetch("second", &second);
    wait_for("the second download", || {
        cache.cached_path(&second).is_some()
    });
    wait_for("the unwatched first item to be recycled", || {
        cache.cached_path(&first).is_none()
    });
}

/// ...but it must never displace something the child actually watched. A guess
/// is dropped rather than made to cost them a film they chose.
#[test]
fn a_prefetch_will_not_displace_a_watched_video() {
    let dir = tempfile::tempdir().unwrap();
    let base = serve(2);
    let watched = http_source(&format!("{base}/watched.mp4"));
    let guess = http_source(&format!("{base}/guess.mp4"));

    let cache = cache_in(dir.path(), BODY.len() as u64);
    cache.queue_prefetch("watched", &watched);
    wait_for("the first download", || {
        cache.cached_path(&watched).is_some()
    });
    cache.mark_played(&watched);

    cache.queue_prefetch("guess", &guess);
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
    cache.queue_prefetch("old", &old);
    wait_for("the first download", || cache.cached_path(&old).is_some());
    cache.mark_played(&old);

    cache.queue_after_play("watched", &watched);
    wait_for("the after-play download", || {
        cache.cached_path(&watched).is_some()
    });
    wait_for("the eviction that follows it", || {
        cache.cached_path(&old).is_none()
    });
}

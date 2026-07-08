//! YouTube resolution via yt-dlp.
//!
//! The parsing here (yt-dlp's `--flat-playlist` NDJSON → playlist entries, and
//! a video's direct stream URL) is pure and host-testable. Actually *running*
//! yt-dlp is abstracted behind the [`YtDlp`] trait: on Android the
//! [`provider`] returns a binding to youtubedl-android (yt-dlp bundled with a
//! Python runtime) over JNI; on other platforms there is no provider, so
//! YouTube sources report "unsupported" exactly as before.
//!
//! Two operations are needed, both mapped onto the same yt-dlp invocation
//! model `(url, options) -> stdout`:
//!
//! - **Playlist**: `--dump-json --flat-playlist` over a playlist URL yields one
//!   JSON object per video; [`parse_flat_playlist`] turns that into entries the
//!   core `build_library_from_entries` assembles into a `Library`.
//! - **Stream**: `-f <fmt> -g` over a watch URL prints the direct media URL we
//!   hand to libmpv (mpv's own ytdl hook can't run — there's no yt-dlp on the
//!   Android PATH).

use shepherd_media_app::Quality;
use shepherd_media_core::{PlaylistInfo, parse_flat_playlist};

/// Runs yt-dlp for a single URL with the given options (yt-dlp CLI flags, minus
/// the URL). Returns yt-dlp's stdout on success, or a human-readable error.
/// Blocking; callers run it off the UI thread.
pub trait YtDlp: Send + Sync {
    fn run(&self, url: &str, options: &[&str]) -> Result<String, String>;
}

/// yt-dlp's `--format` selector for a quality preset.
///
/// YouTube no longer reliably offers a single progressive (muxed) file: the
/// player clients youtubedl-android can use either expose only DASH (separate
/// video-only and audio-only tracks) or an HLS manifest. So we ask for
/// `bv*+ba` — best video up to the height cap *plus* best audio — and let `-g`
/// print the two URLs ([`resolve_stream_url`] hands the audio one to the player
/// as an external track).
///
/// We require an H.264 video track (`vcodec^=avc1`): YouTube's best DASH video
/// is usually VP9/AV1, which fails to decode on many mobile GPUs (audio plays,
/// video stays black), whereas H.264 hardware-decodes reliably. The
/// `/bv*+ba/b` tails fall back to any codec, then a muxed format, if no H.264
/// track exists.
pub fn stream_format(quality: Quality) -> &'static str {
    match quality {
        Quality::Q480 => "bv*[vcodec^=avc1][height<=?480]+ba/bv*[height<=?480]+ba/b[height<=?480]",
        Quality::Q720 | Quality::Q1080 | Quality::Best => {
            "bv*[vcodec^=avc1][height<=?720]+ba/bv*[height<=?720]+ba/b[height<=?720]"
        }
    }
}

/// Fetch and parse a playlist's metadata.
pub fn fetch_playlist(ytdlp: &dyn YtDlp, url: &str) -> Result<PlaylistInfo, String> {
    let stdout = ytdlp.run(
        url,
        &["--dump-json", "--flat-playlist", "--quiet", "--no-warnings"],
    )?;
    parse_flat_playlist(&stdout, url)
}

/// A resolved playable stream: a video URL and, when the format is DASH
/// (separate tracks), the matching audio URL to attach as an external track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamUrls {
    pub video: String,
    pub audio: Option<String>,
}

/// Resolve a watch URL to its playable stream URL(s) for the player.
pub fn resolve_stream_url(
    ytdlp: &dyn YtDlp,
    watch_url: &str,
    quality: Quality,
) -> Result<StreamUrls, String> {
    let stdout = ytdlp.run(
        watch_url,
        &[
            "-f",
            stream_format(quality),
            // `android_vr` serves the full DASH format ladder (incl. H.264 video)
            // without a PO token; the default `android` client returns audio-only
            // for normal videos (PO-token gated) and `web`/`web_safari` need a PO
            // token or only offer HLS. But `android_vr` reports some licensed,
            // DRM-protected videos (e.g. PBS/Muppets "full episode" uploads) as
            // "not available", even though a non-DRM legacy progressive stream
            // (itag 18, 360p H.264) is still served to the `android` client. List
            // both so yt-dlp merges their formats: the selector keeps 720p DASH
            // for normal videos and falls back to the muxed 360p for DRM ones.
            "--extractor-args",
            "youtube:player_client=android_vr,android",
            "-g",
            "--no-playlist",
            "--quiet",
            "--no-warnings",
        ],
    )?;
    parse_stream_urls(&stdout, watch_url)
}

/// Parse `yt-dlp -g` output. With a `bv*+ba` selection it prints two URLs (video
/// then audio); a muxed fallback prints one. The first non-empty line is the
/// video (or muxed) URL; a second, if present, is the audio track.
pub fn parse_stream_urls(stdout: &str, watch_url: &str) -> Result<StreamUrls, String> {
    let mut lines = stdout.lines().map(str::trim).filter(|l| !l.is_empty());
    let video = lines
        .next()
        .map(str::to_string)
        .ok_or_else(|| format!("yt-dlp returned no stream URL for {watch_url}"))?;
    let audio = lines.next().map(str::to_string);
    Ok(StreamUrls { video, audio })
}

/// The yt-dlp provider for this platform, if any.
#[cfg(target_os = "android")]
pub fn provider() -> Option<Box<dyn YtDlp>> {
    Some(Box::new(jni_impl::JniYtDlp))
}

#[cfg(not(target_os = "android"))]
pub fn provider() -> Option<Box<dyn YtDlp>> {
    None
}

/// JNI bridge to youtubedl-android (`com.yausername.youtubedl_android`).
///
/// This path can only be exercised on a device — it depends on the JVM, the
/// youtubedl-android AAR, and its bundled Python being extracted at runtime.
///
/// Classloader gotcha (verified on hardware): this runs on a Rust-spawned
/// worker thread attached to the JVM, where the implicit `FindClass` behind a
/// class-name lookup resolves against the *bootstrap* classloader, which can't
/// see the app's DEX classes — `FindClass("…/YoutubeDL")` throws
/// `ClassNotFoundException`, and the next `NewStringUTF` then aborts the whole
/// process via CheckJNI. The fix is to resolve the classes through the
/// *application* classloader (`Context.getClassLoader().loadClass(…)`) and call
/// through the resulting `jclass`. See [`load_class`].
#[cfg(target_os = "android")]
mod jni_impl {
    use std::io::Read;
    use std::path::Path;
    use std::sync::Once;
    use std::time::{Duration, SystemTime};

    use jni::JavaVM;
    use jni::objects::{JClass, JObject, JString, JValue};

    use super::YtDlp;

    static INIT: Once = Once::new();

    pub struct JniYtDlp;

    impl YtDlp for JniYtDlp {
        fn run(&self, url: &str, options: &[&str]) -> Result<String, String> {
            run_jni(url, options).map_err(|e| format!("youtubedl-android: {e}"))
        }
    }

    fn jni_err(e: jni::errors::Error) -> String {
        format!("JNI error: {e}")
    }

    /// The app's `noBackupFilesDir` (where youtubedl-android stages its runtime),
    /// via `Context.getNoBackupFilesDir().getAbsolutePath()`.
    fn no_backup_dir(env: &mut jni::JNIEnv, context: &JObject) -> Option<std::path::PathBuf> {
        let file = env
            .call_method(context, "getNoBackupFilesDir", "()Ljava/io/File;", &[])
            .ok()?
            .l()
            .ok()?;
        let path = env
            .call_method(&file, "getAbsolutePath", "()Ljava/lang/String;", &[])
            .ok()?
            .l()
            .ok()?;
        let s: String = env.get_string(&JString::from(path)).ok()?.into();
        Some(std::path::PathBuf::from(s))
    }

    /// Refresh the youtubedl-android yt-dlp payload to the latest release.
    ///
    /// The yt-dlp bundled in the AAR is too old to extract video formats from
    /// current YouTube (it returns only audio-only itags), and the library's own
    /// `updateYoutubeDL` throws `ExceptionInInitializerError` on this AAR. So
    /// after `init()` lays down the Python runtime + payload dir, we drop the
    /// latest yt-dlp zipapp from GitHub into the payload path ourselves
    /// (`<noBackupFilesDir>/youtubedl-android/yt-dlp/yt-dlp`). youtubedl-android
    /// runs whatever file is there with its bundled Python and won't clobber it
    /// (it only re-extracts when its own recorded version changes). Best-effort:
    /// skipped when fresh, and any failure (offline, etc.) leaves the existing
    /// payload untouched.
    fn refresh_ytdlp(env: &mut jni::JNIEnv, context: &JObject) {
        const REFRESH_INTERVAL: Duration = Duration::from_secs(7 * 24 * 3600);
        let Some(base) = no_backup_dir(env, context) else {
            log::warn!("yt-dlp refresh: could not resolve noBackupFilesDir");
            return;
        };
        let dir = base.join("youtubedl-android").join("yt-dlp");
        if !dir.is_dir() {
            // init() didn't lay down the payload dir; nothing to refresh.
            return;
        }
        let marker = dir.join(".shepherd-ytdlp-refreshed");
        let fresh = marker
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age < REFRESH_INTERVAL);
        if fresh {
            return;
        }
        match download_ytdlp(&dir.join("yt-dlp")) {
            Ok(()) => {
                let _ = std::fs::write(&marker, b"");
                log::info!("yt-dlp payload refreshed from GitHub");
            }
            Err(e) => log::warn!("yt-dlp refresh failed (keeping existing payload): {e}"),
        }
    }

    /// Download the latest yt-dlp zipapp and atomically replace `payload`.
    fn download_ytdlp(payload: &Path) -> Result<(), String> {
        const URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp";
        const MAX_BYTES: u64 = 20 * 1024 * 1024;
        let resp = ureq::get(URL).call().map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        resp.into_reader()
            .take(MAX_BYTES)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        // The zipapp is a multi-MB file beginning with a `#!` shebang. This
        // guards against a captive-portal HTML page or a truncated download
        // silently corrupting the payload.
        if bytes.len() < 1_000_000 || !bytes.starts_with(b"#!") {
            return Err(format!("unexpected download ({} bytes)", bytes.len()));
        }
        let tmp = payload.with_extension("tmp");
        std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, payload).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// If a Java exception is pending, return its message and **clear** it.
    ///
    /// Critical for correctness: yt-dlp surfaces failures (unavailable format,
    /// private/blocked video, network) as a `YoutubeDLException`. Left pending,
    /// it would propagate uncaught when this Rust-spawned worker thread detaches
    /// and kill the whole process — so any throwing JNI call must funnel through
    /// here to turn the exception into a recoverable `Err`.
    fn take_pending_exception(env: &mut jni::JNIEnv) -> Option<String> {
        if !env.exception_check().unwrap_or(false) {
            return None;
        }
        // Grab the Throwable before clearing — no other JNI call may run while
        // an exception is pending.
        let throwable = env.exception_occurred().ok();
        let _ = env.exception_clear();
        let msg = throwable.and_then(|t| {
            let m = env
                .call_method(&t, "getMessage", "()Ljava/lang/String;", &[])
                .ok()?
                .l()
                .ok()?;
            if m.is_null() {
                return None;
            }
            env.get_string(&JString::from(m)).ok().map(Into::into)
        });
        Some(msg.unwrap_or_else(|| "Java exception".to_string()))
    }

    /// Resolve a class by its dotted name through the application classloader.
    ///
    /// `env.find_class()` (and the class-name forms of `call_static_method` /
    /// `new_object`) use `FindClass`, which on a native worker thread consults
    /// the bootstrap classloader and cannot see app/library classes. Going
    /// through `Context.getClassLoader().loadClass(name)` resolves against the
    /// app's DEX path instead.
    fn load_class<'a>(
        env: &mut jni::JNIEnv<'a>,
        loader: &JObject,
        dotted_name: &str,
    ) -> Result<JClass<'a>, jni::errors::Error> {
        let jname: JString = env.new_string(dotted_name)?;
        let cls = env
            .call_method(
                loader,
                "loadClass",
                "(Ljava/lang/String;)Ljava/lang/Class;",
                &[JValue::Object(&jname)],
            )?
            .l()?;
        Ok(JClass::from(cls))
    }

    fn run_jni(url: &str, options: &[&str]) -> Result<String, String> {
        let ctx = ndk_context::android_context();
        let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.map_err(jni_err)?;
        let mut env = vm.attach_current_thread().map_err(jni_err)?;
        let context = unsafe { JObject::from_raw(ctx.context().cast()) };

        // Resolve the youtubedl-android classes via the *application*
        // classloader (see `load_class`); the implicit `FindClass` would use the
        // bootstrap loader on this worker thread and abort the process.
        let loader = env
            .call_method(&context, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
            .map_err(jni_err)?
            .l()
            .map_err(jni_err)?;
        let youtube_dl = load_class(
            &mut env,
            &loader,
            "com.yausername.youtubedl_android.YoutubeDL",
        )
        .map_err(jni_err)?;
        let request_cls = load_class(
            &mut env,
            &loader,
            "com.yausername.youtubedl_android.YoutubeDLRequest",
        )
        .map_err(jni_err)?;

        // One-time init: extracts the bundled Python/yt-dlp into app storage.
        INIT.call_once(|| {
            let instance = env
                .call_static_method(
                    &youtube_dl,
                    "getInstance",
                    "()Lcom/yausername/youtubedl_android/YoutubeDL;",
                    &[],
                )
                .and_then(|v| v.l());
            match instance {
                Ok(instance) => {
                    let _ = env.call_method(
                        &instance,
                        "init",
                        "(Landroid/content/Context;)V",
                        &[JValue::Object(&context)],
                    );
                    if env.exception_check().unwrap_or(false) {
                        let _ = env.exception_clear();
                        log::error!("YoutubeDL.init threw");
                    }
                    // Replace the AAR's stale bundled yt-dlp with the latest
                    // release so current YouTube formats resolve.
                    refresh_ytdlp(&mut env, &context);
                }
                Err(e) => {
                    // Clear any pending exception so it can't poison the next
                    // JNI call (a pending exception turns NewStringUTF into a
                    // hard CheckJNI abort).
                    let _ = env.exception_clear();
                    log::error!("YoutubeDL.getInstance failed during init: {e}");
                }
            }
        });

        // request = new YoutubeDLRequest(url)
        let jurl: JString = env.new_string(url).map_err(jni_err)?;
        let request = env
            .new_object(
                &request_cls,
                "(Ljava/lang/String;)V",
                &[JValue::Object(&jurl)],
            )
            .map_err(jni_err)?;

        // request.addOption(opt) for each flag (and its value, passed as its
        // own arg — youtubedl-android treats each token as a separate option).
        for opt in options {
            let jopt: JString = env.new_string(opt).map_err(jni_err)?;
            env.call_method(
                &request,
                "addOption",
                "(Ljava/lang/String;)Lcom/yausername/youtubedl_android/YoutubeDLRequest;",
                &[JValue::Object(&jopt)],
            )
            .map_err(jni_err)?;
        }

        // response = YoutubeDL.getInstance().execute(request)
        let instance = env
            .call_static_method(
                &youtube_dl,
                "getInstance",
                "()Lcom/yausername/youtubedl_android/YoutubeDL;",
                &[],
            )
            .map_err(jni_err)?
            .l()
            .map_err(jni_err)?;
        // execute() throws YoutubeDLException on any yt-dlp failure. Catch it
        // and clear the pending exception (see take_pending_exception) so the
        // worker thread doesn't crash the process when it detaches.
        //
        // Use the (request, processId, useCache) overload with useCache=true: the
        // default execute passes `--no-cache-dir`, which makes yt-dlp re-download
        // and re-parse YouTube's player JS (the expensive nsig extraction) on
        // every call. With caching on it writes to the app cache dir, so repeat
        // resolves — and the next launch — reuse the extracted player. The
        // process id must be unique per call: the library rejects a second
        // `execute` with a live id ("Process ID already exists"), and the same
        // item can resolve twice at once (a background prefetch racing the play).
        static RESOLVE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let pid_value = format!(
            "shepherd-resolve-{}",
            RESOLVE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let pid: JString = env.new_string(&pid_value).map_err(jni_err)?;
        let response = match env.call_method(
            &instance,
            "execute",
            "(Lcom/yausername/youtubedl_android/YoutubeDLRequest;Ljava/lang/String;Z)\
             Lcom/yausername/youtubedl_android/YoutubeDLResponse;",
            &[
                JValue::Object(&request),
                JValue::Object(&pid),
                JValue::Bool(1),
            ],
        ) {
            Ok(v) => v.l().map_err(jni_err)?,
            Err(_) => {
                return Err(take_pending_exception(&mut env)
                    .unwrap_or_else(|| "yt-dlp execution failed".to_string()));
            }
        };
        // Defensive: surface (and clear) any exception left pending even when
        // the call reported success.
        if let Some(msg) = take_pending_exception(&mut env) {
            return Err(msg);
        }

        // response.getOut()
        let out = env
            .call_method(&response, "getOut", "()Ljava/lang/String;", &[])
            .map_err(jni_err)?
            .l()
            .map_err(jni_err)?;
        let out: String = env.get_string(&JString::from(out)).map_err(jni_err)?.into();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canned yt-dlp output for tests.
    struct FakeYtDlp(&'static str);
    impl YtDlp for FakeYtDlp {
        fn run(&self, _url: &str, _options: &[&str]) -> Result<String, String> {
            Ok(self.0.to_string())
        }
    }

    const FLAT_JSON: &str = concat!(
        r#"{"id":"abc123","title":"First","duration":61.0,"thumbnail":"https://i.ytimg.com/vi/abc123/hq.jpg","playlist_title":"My List","playlist_id":"PL999"}"#,
        "\n",
        r#"{"id":"def456","title":"Second","duration":120.0}"#,
        "\n",
    );

    // The pure NDJSON parser (parse_flat_playlist) now lives in and is tested
    // by shepherd-media-core; this test covers the Android provider wiring
    // (YtDlp trait → core parser) that the core tests can't reach.
    #[test]
    fn fetch_playlist_uses_the_provider() {
        let fake = FakeYtDlp(FLAT_JSON);
        let info = fetch_playlist(&fake, "https://youtube.com/playlist?list=PL999").unwrap();
        assert_eq!(info.entries.len(), 2);
    }

    #[test]
    fn parses_video_and_audio_urls() {
        let out = "\nhttps://rr3.googlevideo.com/videoplayback?v=1\nhttps://rr3.googlevideo.com/videoplayback?a=1\n";
        let urls = parse_stream_urls(out, "w").unwrap();
        assert_eq!(urls.video, "https://rr3.googlevideo.com/videoplayback?v=1");
        assert_eq!(
            urls.audio.as_deref(),
            Some("https://rr3.googlevideo.com/videoplayback?a=1")
        );
    }

    #[test]
    fn parses_single_muxed_url_without_audio() {
        let out = "https://rr3.googlevideo.com/videoplayback?muxed\n";
        let urls = parse_stream_urls(out, "w").unwrap();
        assert_eq!(
            urls.video,
            "https://rr3.googlevideo.com/videoplayback?muxed"
        );
        assert_eq!(urls.audio, None);
    }

    #[test]
    fn missing_stream_url_is_an_error() {
        assert!(parse_stream_urls("   \n", "w").is_err());
    }

    #[test]
    fn stream_format_selects_video_plus_audio() {
        // Must request a video track (with audio), not a bare `best` that can
        // resolve to an audio-only DASH stream (black screen).
        let q480 = stream_format(Quality::Q480);
        assert!(q480.starts_with("bv*"), "{q480}");
        assert!(q480.contains("+ba"), "{q480}");
        assert!(q480.contains("height<=?480"), "{q480}");
        assert!(stream_format(Quality::Best).contains("height<=?720"));
    }
}

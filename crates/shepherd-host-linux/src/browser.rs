//! Supervised-browser policy materialization for the `com.google.Chrome`
//! Flatpak.
//!
//! Translates a [`BrowserSpec`] into the documented controls we use to wrap
//! Chrome (no patching, no DRM circumvention):
//!
//! 1. A Chromium [managed-policy JSON][policies] file written under the
//!    Flatpak's own per-user config dir, enforcing the URL allow/blocklist and
//!    lockdown switches.
//! 2. A set of Chrome command-line flags (`--kiosk`, `--app=<url>`, …) derived
//!    from the window mode and start URL.
//!
//! **Policy injection is per-user, not system-wide.** Google Chrome only reads
//! managed policy from the root-owned, machine-wide `/etc/opt/chrome/policies/`
//! — writing there would hijack Chrome for every user on the box. The Flatpak's
//! launch wrapper, however, populates that path *inside its own sandbox* (an
//! ephemeral, per-launch filesystem). So instead of touching the host's
//! `/etc`, [`chrome_flatpak_argv`] launches Chrome through a tiny shim that
//! symlinks our per-user policy file into the sandbox's `/etc/opt/chrome/
//! policies/managed/` and then execs the Flatpak's normal entry point. The
//! policy applies only to that launch, only for this user.
//!
//! Profile management is layered on top: [`user_data_dir`] selects a
//! per-`profile_id` on-disk directory passed via `--user-data-dir`, and
//! [`wipe_profile_dir`] removes it after the session ends when `wipe_on_exit`
//! is set.
//!
//! [policies]: https://chromeenterprise.google/policies/

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use shepherd_api::BrowserMode;
use shepherd_host_api::BrowserSpec;

/// The only Flatpak app id this browser support targets. The launch shim and
/// the in-sandbox policy path (`/etc/opt/chrome/...`, `/app/bin/chrome`) are
/// specific to the Flathub Google Chrome package.
pub const SUPPORTED_BROWSER_FLATPAK: &str = "com.google.Chrome";

/// Directory, relative to the user's home, where we stash the policy JSON for
/// the Chrome Flatpak. It lives in the app's own per-user config tree (so it's
/// visible inside the sandbox at the same path) and is symlinked into the
/// sandbox's `/etc/opt/chrome/policies/managed/` at launch — never written to
/// the host's machine-wide `/etc`.
const POLICY_FILE_SUBDIR: &str = ".var/app/com.google.Chrome/config/shepherd-policies";

/// Shell run inside the sandbox by [`chrome_flatpak_argv`]: seed the sandbox's
/// own (ephemeral) managed-policy dir with our file, then hand off to the
/// Flatpak's normal launcher (`/app/bin/chrome`), which re-runs its host-policy
/// merge harmlessly, sets up Widevine, and execs the browser. `$SHEPHERD_POLICY`
/// is passed via `--env=`; `"$@"` is the Chrome flag list.
const POLICY_INJECT_SCRIPT: &str = "mkdir -p /etc/opt/chrome/policies/managed; \
     ln -sf \"$SHEPHERD_POLICY\" /etc/opt/chrome/policies/managed/shepherd.json; \
     exec /app/bin/chrome \"$@\"";

/// Per-profile user-data-dir parent, relative to the user's home, for the
/// com.google.Chrome Flatpak. The path string is identical inside and outside
/// the sandbox (flatpak passes `~/.var/app/<id>` through unchanged), so it can
/// be handed straight to `--user-data-dir`.
const USER_DATA_SUBDIR: &str = ".var/app/com.google.Chrome/config/google-chrome";

/// Reduce an entry id to a safe single-segment filename stem.
fn sanitize_filename(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    // Never let the stem be empty or a path-traversal token.
    match cleaned.as_str() {
        "" | "." | ".." => "entry".to_string(),
        _ => cleaned,
    }
}

/// Build the Chromium managed-policy document for a browser spec.
///
/// A URL allowlist only *restricts* browsing if everything else is blocked, so
/// when `url_allowlist` is non-empty we merge a catch-all `"*"` into
/// `URLBlocklist` (ahead of any user blocklist entries) to make the allowlist
/// authoritative — matching the kiosk intent of the feature.
fn build_policy_json(spec: &BrowserSpec) -> Value {
    let mut map = serde_json::Map::new();

    if !spec.url_allowlist.is_empty() {
        map.insert("URLAllowlist".into(), json!(spec.url_allowlist));
        let mut blocklist = vec!["*".to_string()];
        blocklist.extend(spec.url_blocklist.iter().filter(|b| *b != "*").cloned());
        map.insert("URLBlocklist".into(), json!(blocklist));
    } else if !spec.url_blocklist.is_empty() {
        map.insert("URLBlocklist".into(), json!(spec.url_blocklist));
    }

    if spec.disable_dev_tools {
        // DeveloperToolsAvailability: 2 = disallowed everywhere.
        map.insert("DeveloperToolsAvailability".into(), json!(2));
    }
    if spec.disable_incognito {
        // IncognitoModeAvailability: 1 = incognito disabled.
        map.insert("IncognitoModeAvailability".into(), json!(1));
    }
    if spec.disable_extensions {
        map.insert("ExtensionInstallBlocklist".into(), json!(["*"]));
    }

    Value::Object(map)
}

/// Compute the per-user policy-file path for an entry under `home`.
fn policy_file_path(home: &Path, policy_id: &str) -> PathBuf {
    home.join(POLICY_FILE_SUBDIR)
        .join(format!("{}.json", sanitize_filename(policy_id)))
}

/// Write (or overwrite) the Chromium managed-policy JSON for `spec` under
/// `root` (normally the user's home), returning the path written. The file is
/// later symlinked into the sandbox at launch by [`chrome_flatpak_argv`].
/// Regenerated on every spawn so config edits take effect.
pub fn write_policy_file(root: &Path, spec: &BrowserSpec) -> io::Result<PathBuf> {
    let path = policy_file_path(root, &spec.policy_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(&build_policy_json(spec))?;
    std::fs::write(&path, body)?;
    Ok(path)
}

/// True if `app_id` is the Flatpak this browser support targets.
pub fn is_supported_browser_flatpak(app_id: &str) -> bool {
    app_id == SUPPORTED_BROWSER_FLATPAK
}

/// Build the full `flatpak run …` argv that launches the Chrome Flatpak with
/// our per-user managed policy injected into its sandbox (see the module docs).
///
/// `policy_file` is the host path written by [`write_policy_file`] (visible at
/// the same path inside the sandbox); `env` is the entry's user environment,
/// mirrored as `--env=` flags so the sandboxed app sees it; `chrome_flags` are
/// the browser arguments (`--user-data-dir`, kiosk/app/url).
pub fn chrome_flatpak_argv(
    app_id: &str,
    policy_file: &Path,
    env: &HashMap<String, String>,
    chrome_flags: &[String],
) -> Vec<String> {
    let mut argv = vec![
        "flatpak".to_string(),
        "run".to_string(),
        "--command=bash".to_string(),
        format!("--env=SHEPHERD_POLICY={}", policy_file.display()),
    ];
    // Mirror the user env into the sandbox (sorted for determinism), matching
    // the plain-flatpak spawn path.
    let mut keys: Vec<&String> = env.keys().collect();
    keys.sort();
    for k in keys {
        argv.push(format!("--env={}={}", k, env[k]));
    }
    argv.push(app_id.to_string());
    argv.push("-c".to_string());
    argv.push(POLICY_INJECT_SCRIPT.to_string());
    argv.push("bash".to_string()); // $0 for the inner shell
    argv.extend(chrome_flags.iter().cloned());
    argv
}

/// Compute the per-profile user-data-dir under `home`.
fn user_data_dir_at(home: &Path, profile_id: &str) -> PathBuf {
    home.join(USER_DATA_SUBDIR)
        .join(sanitize_filename(profile_id))
}

/// Resolve the absolute per-profile user-data-dir for `spec` under `root`
/// (normally the user's home). The directory is not created here — Chrome
/// creates it on first launch; we only need the path for the `--user-data-dir`
/// flag and for [`wipe_profile_dir`].
pub fn user_data_dir(root: &Path, spec: &BrowserSpec) -> PathBuf {
    user_data_dir_at(root, &spec.profile_id)
}

/// Remove a per-profile user-data-dir after a session ends (`wipe_on_exit`).
/// A missing directory is fine; other errors are logged, not propagated.
pub fn wipe_profile_dir(dir: &Path) {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => tracing::info!(dir = %dir.display(), "Wiped browser profile"),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            tracing::warn!(error = %e, dir = %dir.display(), "Failed to wipe browser profile")
        }
    }
}

/// Derive the Chrome command-line flags for a browser spec. `user_data_dir`,
/// when provided, becomes `--user-data-dir=<path>` so each profile is isolated
/// on disk. The window mode and start URL follow.
pub fn chrome_flags(spec: &BrowserSpec, user_data_dir: Option<&Path>) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(dir) = user_data_dir {
        args.push(format!("--user-data-dir={}", dir.display()));
    }
    match spec.mode {
        BrowserMode::Kiosk => {
            args.push("--kiosk".to_string());
            if let Some(url) = &spec.start_url {
                args.push(url.clone());
            }
        }
        BrowserMode::App => {
            if let Some(url) = &spec.start_url {
                args.push(format!("--app={url}"));
            } else {
                tracing::warn!(
                    policy_id = %spec.policy_id,
                    "browser.mode = app but no start_url; launching a normal window"
                );
            }
        }
        BrowserMode::Windowed => {
            if let Some(url) = &spec.start_url {
                args.push(url.clone());
            }
        }
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> BrowserSpec {
        BrowserSpec {
            policy_id: "chrome-school".into(),
            profile_id: "school".into(),
            mode: BrowserMode::Kiosk,
            start_url: Some("https://classroom.google.com".into()),
            url_allowlist: vec!["https://*.google.com/*".into()],
            url_blocklist: vec![],
            disable_dev_tools: true,
            disable_incognito: true,
            disable_extensions: true,
            wipe_on_exit: false,
        }
    }

    #[test]
    fn allowlist_injects_catchall_blocklist() {
        let json = build_policy_json(&spec());
        assert_eq!(json["URLAllowlist"], json!(["https://*.google.com/*"]));
        // Allowlist must be authoritative: everything else blocked.
        assert_eq!(json["URLBlocklist"], json!(["*"]));
    }

    #[test]
    fn user_blocklist_merges_after_catchall_without_dup() {
        let mut s = spec();
        s.url_blocklist = vec!["*".into(), "https://evil.example/*".into()];
        let json = build_policy_json(&s);
        assert_eq!(json["URLBlocklist"], json!(["*", "https://evil.example/*"]));
    }

    #[test]
    fn blocklist_only_when_no_allowlist() {
        let mut s = spec();
        s.url_allowlist = vec![];
        s.url_blocklist = vec!["https://evil.example/*".into()];
        let json = build_policy_json(&s);
        assert!(json.get("URLAllowlist").is_none());
        assert_eq!(json["URLBlocklist"], json!(["https://evil.example/*"]));
    }

    /// Golden snapshot of the whole managed-policy document for a fully-locked
    /// spec. Locks the exact policy keys/values (and the `URLBlocklist: ["*"]`
    /// injection) so an accidental change to the security-sensitive mapping
    /// fails loudly rather than silently weakening the kiosk.
    #[test]
    fn policy_json_golden() {
        let mut s = spec();
        s.url_allowlist = vec!["https://*.google.com/*".into()];
        s.url_blocklist = vec!["https://*.google.com/ads/*".into()];
        assert_eq!(
            build_policy_json(&s),
            json!({
                "URLAllowlist": ["https://*.google.com/*"],
                "URLBlocklist": ["*", "https://*.google.com/ads/*"],
                "DeveloperToolsAvailability": 2,
                "IncognitoModeAvailability": 1,
                "ExtensionInstallBlocklist": ["*"],
            })
        );
    }

    #[test]
    fn lockdown_switches_map_to_policies() {
        let json = build_policy_json(&spec());
        assert_eq!(json["DeveloperToolsAvailability"], json!(2));
        assert_eq!(json["IncognitoModeAvailability"], json!(1));
        assert_eq!(json["ExtensionInstallBlocklist"], json!(["*"]));
    }

    #[test]
    fn lockdown_switches_omitted_when_disabled() {
        let mut s = spec();
        s.disable_dev_tools = false;
        s.disable_incognito = false;
        s.disable_extensions = false;
        let json = build_policy_json(&s);
        assert!(json.get("DeveloperToolsAvailability").is_none());
        assert!(json.get("IncognitoModeAvailability").is_none());
        assert!(json.get("ExtensionInstallBlocklist").is_none());
    }

    #[test]
    fn kiosk_flags() {
        assert_eq!(
            chrome_flags(&spec(), None),
            vec![
                "--kiosk".to_string(),
                "https://classroom.google.com".to_string()
            ]
        );
    }

    #[test]
    fn app_flags() {
        let mut s = spec();
        s.mode = BrowserMode::App;
        assert_eq!(
            chrome_flags(&s, None),
            vec!["--app=https://classroom.google.com".to_string()]
        );
    }

    #[test]
    fn app_mode_without_url_emits_no_flags() {
        let mut s = spec();
        s.mode = BrowserMode::App;
        s.start_url = None;
        assert!(chrome_flags(&s, None).is_empty());
    }

    #[test]
    fn windowed_flags_just_url() {
        let mut s = spec();
        s.mode = BrowserMode::Windowed;
        assert_eq!(
            chrome_flags(&s, None),
            vec!["https://classroom.google.com".to_string()]
        );
    }

    #[test]
    fn user_data_dir_flag_comes_first() {
        let dir = Path::new("/home/kid/.var/app/com.google.Chrome/config/google-chrome/school");
        assert_eq!(
            chrome_flags(&spec(), Some(dir)),
            vec![
                "--user-data-dir=/home/kid/.var/app/com.google.Chrome/config/google-chrome/school"
                    .to_string(),
                "--kiosk".to_string(),
                "https://classroom.google.com".to_string()
            ]
        );
    }

    #[test]
    fn user_data_dir_is_per_profile_and_sanitized() {
        assert_eq!(
            user_data_dir_at(Path::new("/home/kid"), "school"),
            Path::new("/home/kid/.var/app/com.google.Chrome/config/google-chrome/school")
        );
        // Defense-in-depth: a traversal-y profile id collapses to one safe
        // component (slashes become underscores) and can't escape the parent.
        assert_eq!(
            user_data_dir_at(Path::new("/home/kid"), "../../etc"),
            Path::new("/home/kid/.var/app/com.google.Chrome/config/google-chrome/.._.._etc")
        );
    }

    #[test]
    fn sanitize_filename_rejects_traversal() {
        assert_eq!(sanitize_filename("a/b"), "a_b");
        assert_eq!(sanitize_filename(".."), "entry");
        assert_eq!(sanitize_filename(""), "entry");
        assert_eq!(sanitize_filename("ok-1.2_3"), "ok-1.2_3");
    }

    #[test]
    fn wipe_profile_dir_removes_tree_and_tolerates_missing() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("shepherd-wipe-test-{}-{n}", std::process::id()));

        // Populate a nested tree, then wipe it.
        std::fs::create_dir_all(root.join("Default/Cache")).unwrap();
        std::fs::write(root.join("Default/Cookies"), b"x").unwrap();
        assert!(root.exists());
        wipe_profile_dir(&root);
        assert!(!root.exists());

        // Wiping an already-absent dir is a no-op (no panic).
        wipe_profile_dir(&root);
    }

    #[test]
    fn policy_file_path_is_under_per_user_config() {
        let p = policy_file_path(Path::new("/home/kid"), "chrome-school");
        assert_eq!(
            p,
            Path::new(
                "/home/kid/.var/app/com.google.Chrome/config/shepherd-policies/chrome-school.json"
            )
        );
    }

    #[test]
    fn chrome_flatpak_argv_injects_policy_and_flags() {
        let mut env = HashMap::new();
        env.insert("FOO".to_string(), "bar".to_string());
        let argv = chrome_flatpak_argv(
            "com.google.Chrome",
            Path::new("/home/kid/.var/app/com.google.Chrome/config/shepherd-policies/e.json"),
            &env,
            &["--user-data-dir=/x".to_string(), "--kiosk".to_string()],
        );
        assert_eq!(&argv[0..3], &["flatpak", "run", "--command=bash"]);
        assert!(argv.contains(
            &"--env=SHEPHERD_POLICY=/home/kid/.var/app/com.google.Chrome/config/shepherd-policies/e.json"
                .to_string()
        ));
        assert!(argv.contains(&"--env=FOO=bar".to_string()));
        // app id precedes the `-c <script> bash <flags>` tail.
        let app_at = argv.iter().position(|a| a == "com.google.Chrome").unwrap();
        assert_eq!(argv[app_at + 1], "-c");
        assert_eq!(argv[app_at + 2], POLICY_INJECT_SCRIPT);
        assert_eq!(argv[app_at + 3], "bash");
        assert_eq!(&argv[app_at + 4..], &["--user-data-dir=/x", "--kiosk"]);
        // The shim seeds the sandbox's own /etc, never the host's.
        assert!(POLICY_INJECT_SCRIPT.contains("/etc/opt/chrome/policies/managed"));
        assert!(POLICY_INJECT_SCRIPT.contains("exec /app/bin/chrome"));
    }

    #[test]
    fn supported_flatpak_gate() {
        assert!(is_supported_browser_flatpak("com.google.Chrome"));
        assert!(!is_supported_browser_flatpak("org.chromium.Chromium"));
    }

    // --- Real-Chrome gated test (manual; needs the com.google.Chrome flatpak) ---
    //
    // Everything above verifies the shepherd side. This test verifies what only
    // real Chrome can confirm: that the per-user policy injection
    // ([`chrome_flatpak_argv`]) is actually honored (URL allow/blocklist
    // enforced) **without** touching the host's machine-wide /etc, and that
    // `--user-data-dir` lands where we later wipe.
    //
    // It is `#[ignore]` and self-skips when the flatpak isn't installed, so a
    // normal `cargo test` never touches it. Run via
    // `scripts/integration-tests/test-browser-flatpak.sh`, or directly:
    //   cargo test -p shepherd-host-linux --lib -- --ignored --nocapture \
    //       browser::tests::real_flatpak_chrome
    //
    // Mechanism: build the real launch argv with `chrome_flatpak_argv` and add
    // headless `--dump-dom` flags. The policy + profile live in the *real*
    // ~/.var/app/com.google.Chrome (under throwaway ids, cleaned up) — the
    // injection targets the sandbox's own /etc, so the host /etc is never
    // written. Two loopback servers each serve a marker; only one origin is
    // allowlisted. Policy honored ⇒ the blocked origin renders the interstitial
    // (no marker); not honored ⇒ its marker appears and the test fails.

    fn chrome_skip_reason(app_id: &str) -> Option<String> {
        use std::process::Command;
        if Command::new("flatpak").arg("--version").output().is_err() {
            return Some("flatpak CLI not on PATH".into());
        }
        let installed = Command::new("flatpak")
            .args(["info", app_id])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !installed {
            return Some(format!(
                "flatpak '{app_id}' not installed (run: flatpak install -y flathub {app_id})"
            ));
        }
        None
    }

    /// A throwaway loopback HTTP server that answers every request with an HTML
    /// page containing `marker`. Returns the bound port; threads are detached
    /// and die with the test process.
    ///
    /// Each connection is handled on its own thread with a read timeout —
    /// headless Chrome opens speculative/preconnect sockets that send nothing,
    /// and a single-threaded blocking `read()` would wedge on those and never
    /// serve the real request.
    fn spawn_marker_server(marker: &'static str) -> std::io::Result<u16> {
        use std::io::{Read, Write};
        use std::time::Duration;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let mut buf = [0u8; 1024];
                    let _ = stream.read(&mut buf); // ignore timeout/EOF on idle sockets
                    let body = format!(
                        "<!doctype html><html><head><title>{marker}</title></head>\
                         <body>{marker}</body></html>"
                    );
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.flush();
                });
            }
        });
        Ok(port)
    }

    /// Run the real Chrome Flatpak (via `chrome_flatpak_argv`) headless against
    /// `url`, returning the dumped DOM. `timeout` guards a stuck launch.
    fn run_headless_chrome(app_id: &str, policy_file: &Path, udd: &Path, url: &str) -> String {
        use std::process::Command;
        let flags = vec![
            format!("--user-data-dir={}", udd.display()),
            "--headless=new".to_string(),
            "--disable-gpu".to_string(),
            "--no-first-run".to_string(),
            "--dump-dom".to_string(),
            url.to_string(),
        ];
        let argv = chrome_flatpak_argv(app_id, policy_file, &HashMap::new(), &flags);
        let out = Command::new("timeout")
            .arg("60")
            .args(&argv)
            .output()
            .expect("exec flatpak run");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    #[ignore]
    fn real_flatpak_chrome_enforces_policy_and_user_data_dir() {
        let app_id =
            std::env::var("SHEPHERD_CHROME_FLATPAK").unwrap_or_else(|_| "com.google.Chrome".into());
        if let Some(reason) = chrome_skip_reason(&app_id) {
            eprintln!("[SKIP] real_flatpak_chrome_enforces_policy_and_user_data_dir: {reason}");
            return;
        }

        // The policy + profile live under the REAL home (so they're visible
        // inside the sandbox), under throwaway ids cleaned up below.
        let home = PathBuf::from(std::env::var("HOME").expect("HOME set"));
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let uid = format!(
            "shepherd-real-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );

        let allow_port = spawn_marker_server("SHEPHERDALLOWMARK").unwrap();
        let block_port = spawn_marker_server("SHEPHERDBLOCKMARK").unwrap();

        let spec = BrowserSpec {
            policy_id: uid.clone(),
            profile_id: uid.clone(),
            mode: BrowserMode::Windowed,
            start_url: None,
            // Allowlist only the allow origin; build_policy_json adds the
            // catch-all "*" blocklist, so the block origin must be refused.
            url_allowlist: vec![format!("http://127.0.0.1:{allow_port}")],
            url_blocklist: vec![],
            // NB: `DeveloperToolsAvailability=2` (what disable_dev_tools emits)
            // breaks this headless probe — headless Chrome drives itself over
            // the DevTools protocol, so `--dump-dom` produces nothing. It is
            // correct and desirable for the real (windowed/kiosk) launch, so we
            // simply omit it here and still exercise the other lockdowns.
            disable_dev_tools: false,
            disable_incognito: true,
            disable_extensions: true,
            wipe_on_exit: false,
        };
        let policy_path = write_policy_file(&home, &spec).expect("write policy file");
        let udd = user_data_dir(&home, &spec);

        let allow_dom = run_headless_chrome(
            &app_id,
            &policy_path,
            &udd,
            &format!("http://127.0.0.1:{allow_port}/"),
        );
        let block_dom = run_headless_chrome(
            &app_id,
            &policy_path,
            &udd,
            &format!("http://127.0.0.1:{block_port}/"),
        );

        // Capture outcomes, then clean up before asserting (so a failure still
        // leaves the home tidy). The host /etc must never have been written.
        let host_etc_touched = Path::new("/etc/opt/chrome").exists();
        let udd_created = udd.exists();
        wipe_profile_dir(&udd);
        let udd_wiped = !udd.exists();
        let _ = std::fs::remove_file(&policy_path);

        eprintln!(
            "---- allow DOM ----\n{allow_dom}\n---- block DOM ----\n{block_dom}\n-------------------"
        );

        assert!(
            allow_dom.contains("SHEPHERDALLOWMARK"),
            "allowlisted origin did NOT render — Chrome failed to launch or the policy over-blocks. allow DOM:\n{allow_dom}"
        );
        assert!(
            !block_dom.contains("SHEPHERDBLOCKMARK"),
            "blocked origin rendered — URL allow/blocklist NOT enforced, so the per-user policy \
             injection failed. block DOM:\n{block_dom}"
        );
        assert!(
            !host_etc_touched,
            "injection must not create host /etc/opt/chrome"
        );
        assert!(
            udd_created,
            "Chrome did not create --user-data-dir at {}",
            udd.display()
        );
        assert!(udd_wiped, "wipe failed");
    }
}

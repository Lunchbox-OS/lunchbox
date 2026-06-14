//! Supervised-browser policy materialization.
//!
//! Translates a [`BrowserSpec`] into the two documented controls we use to
//! wrap Chrome (no patching, no DRM circumvention):
//!
//! 1. A Chromium [managed-policy JSON][policies] file written under the
//!    com.google.Chrome Flatpak's per-app config dir, enforcing the URL
//!    allow/blocklist and lockdown switches.
//! 2. A set of Chrome command-line flags (`--kiosk`, `--app=<url>`, …) derived
//!    from the window mode and start URL.
//!
//! Profile management is layered on top: [`user_data_dir`] selects a
//! per-`profile_id` on-disk directory passed via `--user-data-dir`, and
//! [`wipe_profile_dir`] removes it after the session ends when
//! `wipe_on_exit` is set.
//!
//! [policies]: https://chromeenterprise.google/policies/

use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use shepherd_api::BrowserMode;
use shepherd_host_api::BrowserSpec;

/// Managed-policy directory, relative to the user's home, for the official
/// `com.google.Chrome` Flatpak. Chrome inside the sandbox sees this as
/// `$XDG_CONFIG_HOME/chromium/policies/managed`.
///
/// NOTE: this path is the one documented in the design and is the single knob
/// to adjust if a real Flatpak Chrome turns out to read managed policy from a
/// different location (verified during manual on-device testing).
const MANAGED_POLICY_SUBDIR: &str = ".var/app/com.google.Chrome/config/chromium/policies/managed";

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

/// Compute the managed-policy file path for an entry under `home`.
fn managed_policy_path(home: &Path, policy_id: &str) -> PathBuf {
    home.join(MANAGED_POLICY_SUBDIR)
        .join(format!("{}.json", sanitize_filename(policy_id)))
}

/// Write (or overwrite) the Chromium managed-policy JSON for `spec` under
/// `root` (normally the user's home), returning the path written. Regenerated
/// on every spawn so config edits take effect.
pub fn write_managed_policy(root: &Path, spec: &BrowserSpec) -> io::Result<PathBuf> {
    let path = managed_policy_path(root, &spec.policy_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(&build_policy_json(spec))?;
    std::fs::write(&path, body)?;
    Ok(path)
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
    fn managed_policy_path_is_under_chrome_config() {
        let p = managed_policy_path(Path::new("/home/kid"), "chrome-school");
        assert_eq!(
            p,
            Path::new(
                "/home/kid/.var/app/com.google.Chrome/config/chromium/policies/managed/chrome-school.json"
            )
        );
    }

    // --- Real-Chrome gated test (manual; needs the com.google.Chrome flatpak) ---
    //
    // Everything above verifies the shepherd side. This test verifies the two
    // assumptions only real Chrome can confirm: that our managed-policy path is
    // the one Flatpak Chrome actually reads (so the URL allow/blocklist is
    // enforced), and that `--user-data-dir` lands where we later wipe.
    //
    // It is `#[ignore]` and self-skips when the flatpak isn't installed, so a
    // normal `cargo test` run never touches it. Run via
    // `scripts/integration-tests/test-browser-flatpak.sh`, or directly:
    //   cargo test -p shepherd-host-linux --lib -- --ignored --nocapture \
    //       browser::tests::real_flatpak_chrome
    //
    // Mechanism: HOME is redirected to a tempdir so `~/.var/app/com.google.Chrome`
    // (and thus the managed-policy dir + the profile) live under the test root,
    // never touching the user's real Chrome config. XDG_DATA_HOME stays at the
    // real `~/.local/share` so `flatpak run` still finds the user-installed app.
    // Two loopback HTTP servers both serve a unique marker; only one origin is
    // allowlisted. If the policy is read, the blocked origin renders the policy
    // interstitial (no marker); if it isn't, the blocked page renders the marker
    // and the test fails — pointing at a wrong path const.

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
    /// page containing `marker`. Returns the bound port; the thread is detached
    /// and dies with the test process.
    fn spawn_marker_server(marker: &'static str) -> std::io::Result<u16> {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
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
            }
        });
        Ok(port)
    }

    /// Run flatpak Chrome headless against `url` with HOME redirected to `root`,
    /// returning the dumped DOM. Wrapped in `timeout` so a stuck launch can't
    /// hang the suite.
    fn run_headless_chrome(
        app_id: &str,
        root: &Path,
        xdg_data_home: &str,
        udd: &Path,
        url: &str,
    ) -> String {
        use std::process::Command;
        let out = Command::new("timeout")
            .arg("60")
            .arg("flatpak")
            .arg("run")
            .arg(app_id)
            .arg("--headless=new")
            .arg("--disable-gpu")
            .arg("--no-first-run")
            .arg(format!("--user-data-dir={}", udd.display()))
            .arg("--dump-dom")
            .arg(url)
            .env("HOME", root)
            .env("XDG_DATA_HOME", xdg_data_home)
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

        // Redirected HOME root (never the user's real ~/.var/app/...).
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("shepherd-chrome-real-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();

        // flatpak finds the user-installed app via the *real* XDG_DATA_HOME.
        let real_home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
        let xdg_data_home =
            std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{real_home}/.local/share"));

        let allow_port = spawn_marker_server("SHEPHERD_ALLOW_MARKER").unwrap();
        let block_port = spawn_marker_server("SHEPHERD_BLOCK_MARKER").unwrap();

        let spec = BrowserSpec {
            policy_id: "shepherd-real-test".into(),
            profile_id: "realtest".into(),
            mode: BrowserMode::Windowed,
            start_url: None,
            // Allowlist only the allow origin; build_policy_json adds the
            // catch-all "*" blocklist, so the block origin must be refused.
            url_allowlist: vec![format!("127.0.0.1:{allow_port}")],
            url_blocklist: vec![],
            disable_dev_tools: true,
            disable_incognito: true,
            disable_extensions: true,
            wipe_on_exit: false,
        };
        let policy_path = write_managed_policy(&root, &spec).expect("write managed policy");
        let udd = user_data_dir(&root, &spec);

        let allow_dom = run_headless_chrome(
            &app_id,
            &root,
            &xdg_data_home,
            &udd,
            &format!("http://127.0.0.1:{allow_port}/"),
        );
        let block_dom = run_headless_chrome(
            &app_id,
            &root,
            &xdg_data_home,
            &udd,
            &format!("http://127.0.0.1:{block_port}/"),
        );
        eprintln!(
            "---- allow DOM ----\n{allow_dom}\n---- block DOM ----\n{block_dom}\n-------------------"
        );

        assert!(
            allow_dom.contains("SHEPHERD_ALLOW_MARKER"),
            "allowlisted origin did NOT render. Either Chrome failed to launch, or the managed \
             policy at {} was read and is over-blocking. allow DOM:\n{allow_dom}",
            policy_path.display()
        );
        assert!(
            !block_dom.contains("SHEPHERD_BLOCK_MARKER"),
            "blocked origin rendered the page — URLAllowlist/URLBlocklist was NOT enforced, so \
             Chrome did not read the managed policy at {}. The MANAGED_POLICY_SUBDIR const is \
             likely wrong for this Chrome. block DOM:\n{block_dom}",
            policy_path.display()
        );

        // Chrome must have created the profile exactly where we wipe.
        assert!(
            udd.exists(),
            "Chrome did not create --user-data-dir at {}; USER_DATA_SUBDIR may be wrong",
            udd.display()
        );
        wipe_profile_dir(&udd);
        assert!(!udd.exists(), "wipe failed to remove {}", udd.display());

        let _ = std::fs::remove_dir_all(&root);
    }
}

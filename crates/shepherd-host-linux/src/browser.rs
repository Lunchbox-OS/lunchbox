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

/// Write (or overwrite) the Chromium managed-policy JSON for `spec`, returning
/// the path written. Regenerated on every spawn so config edits take effect.
pub fn write_managed_policy(spec: &BrowserSpec) -> io::Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))?;
    let path = managed_policy_path(&home, &spec.policy_id);
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

/// Resolve the absolute per-profile user-data-dir for `spec`. The directory is
/// not created here — Chrome creates it on first launch; we only need the path
/// for the `--user-data-dir` flag and for [`wipe_profile_dir`].
pub fn user_data_dir(spec: &BrowserSpec) -> io::Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))?;
    Ok(user_data_dir_at(&home, &spec.profile_id))
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
}

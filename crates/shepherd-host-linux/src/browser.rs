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
//! Profile management (`--user-data-dir`, `wipe_on_exit`) is layered on top in
//! a later step; this module only covers policy + launch flags.
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

/// Derive the Chrome command-line flags for a browser spec's window mode and
/// start URL. These are appended to the app's argument list at spawn time.
pub fn chrome_flags(spec: &BrowserSpec) -> Vec<String> {
    let mut args = Vec::new();
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
            chrome_flags(&spec()),
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
            chrome_flags(&s),
            vec!["--app=https://classroom.google.com".to_string()]
        );
    }

    #[test]
    fn app_mode_without_url_emits_no_flags() {
        let mut s = spec();
        s.mode = BrowserMode::App;
        s.start_url = None;
        assert!(chrome_flags(&s).is_empty());
    }

    #[test]
    fn windowed_flags_just_url() {
        let mut s = spec();
        s.mode = BrowserMode::Windowed;
        assert_eq!(
            chrome_flags(&s),
            vec!["https://classroom.google.com".to_string()]
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

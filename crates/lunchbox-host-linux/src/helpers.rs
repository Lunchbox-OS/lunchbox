//! Where shepherd's helper binaries come from (issue #144).
//!
//! `lunchboxd` execs a good deal that it did not write — `systemd-run`,
//! `pkexec`, `snap`, `pgrep`, `wpctl` and a dozen more. Every one of them used
//! to be named bare and resolved through `$PATH`, which on a stock 26.04 + GDM
//! host is **chosen by the kiosk user**: `/etc/pam.d/gdm-password` carries
//! `pam_env.so … user_readenv=1`, and `libpam-modules` still honours it, so
//! `~/.pam_environment` sets the session's environment outright. Every activity
//! runs as that uid, so any of them could write one file, drop a `systemd-run`
//! on the resulting `PATH`, and at the next login have shepherd exec it — as a
//! direct child of the daemon, in the daemon's own cgroup, which the management
//! socket accepts as `Admin`. The same substitution turns
//! [`crate::user_scope_argv_prefix`] into a no-op, so every activity lands in
//! shepherd's cgroup too. Nothing fails loudly; the peer check simply stops
//! separating anything.
//!
//! Sanitising the inherited `PATH` would not fix that, because the environment
//! is attacker-chosen wholesale rather than merely untidy. The daemon has to
//! stop reading it for this at all, which is what this module does: helper
//! names are resolved against a **compiled-in** list of root-owned directories,
//! and the result is an absolute path.
//!
//! The measurements behind this are in
//! `docs/ai/history/2026-08-29 004 ipc-peer-cgroup-hole-hunt.md`.
//!
//! ## What this does not cover
//!
//! An activity's own command, from `[entries]` in `config.toml`, is still
//! resolved however the admin wrote it. That file is owned by the same uid the
//! activities run as, so it is the same class of problem — tracked separately
//! as #156/#157, and a policy decision rather than a lookup bug.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, RwLock};

use tracing::{debug, warn};

/// The directories a helper may be loaded from, in order.
///
/// Every one is root-owned on a supported host — verified, not assumed:
/// `/sbin` and `/bin` show `777` only because they are symlinks into `/usr`,
/// whose targets are `755 root:root`. Deliberately compiled in rather than read
/// from anywhere: a list that can be configured is a list an activity can
/// configure.
const TRUSTED_PATH: &[&str] = &[
    "/usr/local/sbin",
    "/usr/local/bin",
    "/usr/sbin",
    "/usr/bin",
    "/sbin",
    "/bin",
    "/snap/bin",
];

/// Where a name that resolves nowhere is pointed instead.
///
/// Returning the bare name would hand the lookup back to `$PATH` — the whole
/// hole. Returning an absolute path under a root-owned directory keeps the
/// failure identical to what a missing tool always produced (`ENOENT` at spawn,
/// logged by the caller) while making it impossible for an activity to satisfy.
/// `pactl` and `wl-mirror` are genuinely optional, so this is a normal path,
/// not an error one.
const FALLBACK_DIR: &str = "/usr/bin";

/// Whether `SHEPHERD_*_BIN`-style environment overrides are honoured.
///
/// Off by default. They are a direct binary-substitution primitive, and the
/// environment is exactly what an activity can control — so on a device they
/// must not be read at all. `lunchboxd` turns them on for a development session,
/// from the same flag that disarms the peer check, because both mean "this
/// stack is not a device and its environment is the developer's".
static TRUST_ENVIRONMENT: RwLock<bool> = RwLock::new(false);

/// Let environment overrides select binaries. Development only.
pub fn set_trust_environment(trust: bool) {
    *TRUST_ENVIRONMENT.write().expect("trust-env lock") = trust;
    if trust {
        warn!(
            "Environment overrides for helper binaries are enabled; this is for development \
             only and must never be set on a device (issue #144)"
        );
    }
}

/// Whether environment overrides are honoured right now.
pub fn environment_is_trusted() -> bool {
    *TRUST_ENVIRONMENT.read().expect("trust-env lock")
}

/// Read `var` as a path override, or `None` when the environment is not trusted.
///
/// The single gate every `SHEPHERD_*_BIN`-style override goes through, so
/// adding one cannot accidentally reintroduce the hole.
pub fn env_override(var: &str) -> Option<PathBuf> {
    if !environment_is_trusted() {
        return None;
    }
    std::env::var(var)
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn cache() -> &'static Mutex<HashMap<String, PathBuf>> {
    static CACHE: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The absolute path to helper `name`, found only in [`TRUSTED_PATH`].
///
/// A `name` that already contains a `/` is returned untouched — it is a caller's
/// deliberate absolute or relative path, not a lookup.
///
/// Resolved lazily and cached: eager resolution at startup would have to decide
/// what to do about the optional helpers that are legitimately absent, and the
/// answer would be "nothing", which is what laziness gives for free.
pub fn resolve(name: &str) -> PathBuf {
    if name.contains('/') {
        return PathBuf::from(name);
    }
    if let Some(hit) = cache().lock().expect("helper cache").get(name) {
        return hit.clone();
    }

    // In a development session `$PATH` is searched first, which is exactly the
    // behaviour from before #144. It has to be: stubbing a helper by putting a
    // fake one on `$PATH` is how the e2e suite tests the flatpak and polkit
    // paths without installing either, and resolving only from `/usr/bin` broke
    // that. A device never takes this branch — `set_trust_environment` is off
    // unless `--no-restrict-ipc-peers` was passed, and `shepherd install
    // sway-config` strips that flag.
    let resolved = environment_is_trusted()
        .then(|| search_path(name))
        .flatten()
        .or_else(|| search_trusted(name))
        .unwrap_or_else(|| {
            debug!(
                helper = name,
                "Not found in any trusted directory; spawning it will fail as a missing tool"
            );
            Path::new(FALLBACK_DIR).join(name)
        });

    cache()
        .lock()
        .expect("helper cache")
        .insert(name.to_string(), resolved.clone());
    resolved
}

/// The absolute path to one of shepherd's **own** binaries.
///
/// A sibling of the running daemon first, which is where both an install and a
/// `cargo build` put them, then a trusted system directory. Never the bare
/// name: these are direct children of the daemon, so they inherit its cgroup
/// and are accepted on the management socket (issue #144).
pub fn resolve_daemon_sibling(name: &str) -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(name);
        if candidate.exists() {
            return candidate;
        }
    }
    resolve(name)
}

/// A [`std::process::Command`] for helper `name`, resolved (issue #144).
///
/// The way shepherd should spawn anything it chose itself. `Command::new` is
/// banned workspace-wide (see `clippy.toml`) precisely so that reaching for it
/// is a deliberate act with a comment attached, rather than the default.
#[allow(clippy::disallowed_methods)]
pub fn command(name: &str) -> std::process::Command {
    std::process::Command::new(resolve(name))
}

/// [`command`], for the async call sites.
#[allow(clippy::disallowed_methods)]
pub fn tokio_command(name: &str) -> tokio::process::Command {
    tokio::process::Command::new(resolve(name))
}

/// [`resolve`], as a `String`, for the argv vectors built by `process.rs`.
pub fn resolve_arg(name: &str) -> String {
    resolve(name).to_string_lossy().into_owned()
}

/// First executable `name` in [`TRUSTED_PATH`].
fn search_trusted(name: &str) -> Option<PathBuf> {
    TRUSTED_PATH
        .iter()
        .map(|dir| Path::new(dir).join(name))
        .find(|p| is_executable(p))
}

/// First executable `name` on `$PATH`. Development only — see [`resolve`].
fn search_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|p| is_executable(p))
    })
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Forget every cached lookup. For tests, and for a helper installed after the
/// daemon started.
pub fn clear_cache() {
    cache().lock().expect("helper cache").clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_trusted_directory_is_root_owned() {
        // The load-bearing assumption. If a distribution ever ships one of
        // these writable by a non-root user, resolving from it is no better
        // than resolving from `$PATH`, and this test is where that is noticed.
        use std::os::unix::fs::MetadataExt;
        for dir in TRUSTED_PATH {
            let Ok(meta) = std::fs::metadata(dir) else {
                continue; // not all of them exist everywhere
            };
            assert_eq!(
                meta.uid(),
                0,
                "{dir} is not root-owned, so it cannot be trusted to hold helpers"
            );
            assert_eq!(
                meta.mode() & 0o022,
                0,
                "{dir} is group- or world-writable, so anything could put a helper there"
            );
        }
    }

    #[test]
    fn a_helper_resolves_to_an_absolute_path_under_a_trusted_directory() {
        // `sh` is the one thing guaranteed present on any host that can build
        // this crate.
        let p = resolve("sh");
        assert!(p.is_absolute(), "resolved to {p:?}, which is not absolute");
        assert!(
            TRUSTED_PATH.iter().any(|d| p.starts_with(d)),
            "resolved to {p:?}, outside every trusted directory"
        );
    }

    #[test]
    fn a_missing_helper_never_falls_back_to_path_lookup() {
        // The important negative. Returning the bare name here would hand the
        // lookup back to an environment an activity controls, which is the
        // entire bug; an absolute path under a root-owned directory fails the
        // same way a missing tool always did.
        let p = resolve("shepherd-no-such-helper-exists");
        assert!(p.is_absolute(), "missing helper resolved to a bare name");
        assert_eq!(p.parent().unwrap(), Path::new(FALLBACK_DIR));
    }

    #[test]
    fn an_explicit_path_is_left_alone() {
        assert_eq!(
            resolve("/opt/thing/bin/tool"),
            PathBuf::from("/opt/thing/bin/tool")
        );
        assert_eq!(
            resolve("./target/debug/thing"),
            PathBuf::from("./target/debug/thing")
        );
    }

    #[test]
    fn environment_overrides_are_refused_unless_development_says_otherwise() {
        // Default-off is the property that matters: a device that never calls
        // `set_trust_environment` cannot be steered by `SHEPHERD_*_BIN`, which
        // is what an activity would reach for after `~/.pam_environment`.
        unsafe { std::env::set_var("SHEPHERD_TEST_HELPER_BIN", "/home/kiosk/evil") };
        set_trust_environment(false);
        assert_eq!(env_override("SHEPHERD_TEST_HELPER_BIN"), None);

        set_trust_environment(true);
        assert_eq!(
            env_override("SHEPHERD_TEST_HELPER_BIN"),
            Some(PathBuf::from("/home/kiosk/evil"))
        );
        set_trust_environment(false);
        unsafe { std::env::remove_var("SHEPHERD_TEST_HELPER_BIN") };
    }
}

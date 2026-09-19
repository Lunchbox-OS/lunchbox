//! The systemd units, the polkit rule and this crate's constants describe the
//! same socket, the same directory and the same uid.
//!
//! Four facts are stated twice and cannot be stated once, because neither
//! systemd nor polkit can call Rust: where the socket is, where the state
//! directory is, which uid owns it, and which uid may end a session. The unit
//! files say the first three in `ListenStream=`, `StateDirectory=` and `User=`,
//! and `50-lunchbox-session-guard.rules` says the fourth; this crate says them
//! in [`socket_path`], [`state_dir`] and [`STATE_USER`].
//!
//! A drift between them fails in the least helpful way available. The service
//! manager binds one path and `lunchboxd` dials another, so the connection is
//! refused, `lunchboxd` falls back to an unprotected local store, and the only
//! symptom is a diagnostic saying the custodian could not be reached — which
//! reads exactly like a custodian that is not installed. Nothing points at the
//! two files that disagree.
//!
//! So they are compared here instead, which is the same bargain the rest of
//! this issue makes: `install sway-config` fails the install if a flag strip
//! did not take, and `headless.sh` dies if `sway.conf` stops passing
//! `--no-state-custodian`. A duplication that must exist gets something that
//! breaks loudly when it drifts.

use lunchbox_state_proto::{STATE_USER, admin_dir, socket_path, state_dir};

fn unit(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../dist/systemd")
        .join(name)
        .canonicalize()
        .unwrap_or_else(|e| panic!("dist/systemd/{name} is where packaging expects it: {e}"));
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {name}: {e}"))
}

/// The value of `key=` in a unit file, ignoring comments.
fn directive(unit: &str, key: &str) -> String {
    unit.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("no {key}= in the unit"))
        .trim()
        .to_string()
}

#[test]
fn the_socket_unit_listens_where_the_client_dials() {
    let listen = directive(&unit("lunchbox-stated@.socket"), "ListenStream");

    // `%i` is systemd's instance name, which is the kiosk user — so the unit
    // and the constant agree exactly when substituting one gives the other.
    let expected = socket_path("%i");
    assert_eq!(
        std::path::Path::new(&listen),
        expected,
        "lunchbox-stated@.socket listens on {listen}, but lunchbox_state_proto::socket_path \
         dials {}. lunchboxd would fall back to an unprotected store and report only that \
         the custodian could not be reached.",
        expected.display()
    );
}

#[test]
fn the_service_unit_owns_the_directory_the_client_stats() {
    let service = unit("lunchbox-stated@.service");

    // `StateDirectory=` is relative to /var/lib, which is the one part the unit
    // does not spell out, and it now names two directories: this user's and the
    // device's. Both have to be created, or the daemon writes into a path the
    // service manager never made.
    let directories: Vec<std::path::PathBuf> = directive(&service, "StateDirectory")
        .split_whitespace()
        .map(|d| std::path::Path::new("/var/lib").join(d))
        .collect();
    assert!(
        directories.contains(&admin_dir()),
        "lunchbox-stated@.service does not create {}, where the admin record, the unbond \
         queue and the reset sentinel live — the daemon would write into a directory \
         nothing made. It creates: {directories:?}",
        admin_dir().display()
    );
    let absolute = directories
        .first()
        .expect("StateDirectory= names at least one directory")
        .clone();
    assert_eq!(
        absolute,
        state_dir("%i"),
        "lunchbox-stated@.service keeps state in {}, but lunchbox_state_proto::state_dir \
         looks in {}. lunchboxd stats that path to tell a broken custodian apart from a \
         device that never had one, so a drift here makes a protected device look fresh — \
         and a fresh device is one the next phone may claim.",
        absolute.display(),
        state_dir("%i").display()
    );

    for key in ["User", "Group"] {
        assert_eq!(
            directive(&service, key),
            STATE_USER,
            "lunchbox-stated@.service runs as a different {key} than \
             lunchbox_state_proto::STATE_USER, which the client checks the socket's owner \
             against before it will speak to it"
        );
    }

    // The mode is the boundary itself rather than a detail: the directory being
    // unreadable to the kiosk uid is the whole mechanism this issue turns on.
    assert_eq!(
        directive(&service, "StateDirectoryMode"),
        "0700",
        "the custodian's directory must stay unreadable to the uid activities run as"
    );
}

/// The polkit rule and the unit have to name the same uid (issue #172).
///
/// A fourth fact stated twice for the same reason as the other three: polkit
/// cannot call Rust either. The rule grants a *user name* the right to end a
/// session; the unit decides which user the daemon actually runs as. Rename one
/// and the watchdog keeps running, keeps counting, fires — and is refused, on a
/// device whose journal will say only that `TerminateSession` was not
/// authorised.
///
/// The check is deliberately crude — the file has to mention the action and the
/// user — because anything cleverer would be re-implementing polkit's JavaScript
/// to catch a rename.
#[test]
fn the_polkit_rule_names_the_user_the_daemon_runs_as() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../dist/polkit/50-lunchbox-session-guard.rules")
        .canonicalize()
        .expect("dist/polkit/50-lunchbox-session-guard.rules is where packaging expects it");
    let rule = std::fs::read_to_string(&path).expect("reading the polkit rule");
    let granting: String = rule
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect();

    assert!(
        granting.contains("org.freedesktop.login1.manage"),
        "the session watchdog's polkit rule no longer grants the action that ends a session, \
         so the watchdog would notice a killed lunchboxd and then be refused"
    );
    assert!(
        granting.contains(&format!("\"{STATE_USER}\"")),
        "the polkit rule does not name {STATE_USER}, which is the user \
         lunchbox-stated@.service runs as -- so the grant applies to nobody and the watchdog \
         cannot end a session"
    );

    let service = unit("lunchbox-stated@.service");
    assert_eq!(
        directive(&service, "User"),
        STATE_USER,
        "the unit runs as a user the polkit rule does not grant"
    );
}

//! The systemd units and this crate's constants describe the same socket.
//!
//! Three facts are stated twice and cannot be stated once, because systemd
//! cannot call Rust: where the socket is, where the state directory is, and
//! which uid owns it. The unit files say them in `ListenStream=`,
//! `StateDirectory=` and `User=`; this crate says them in [`socket_path`],
//! [`state_dir`] and [`STATE_USER`].
//!
//! A drift between them fails in the least helpful way available. The service
//! manager binds one path and `shepherdd` dials another, so the connection is
//! refused, `shepherdd` falls back to an unprotected local store, and the only
//! symptom is a diagnostic saying the custodian could not be reached — which
//! reads exactly like a custodian that is not installed. Nothing points at the
//! two files that disagree.
//!
//! So they are compared here instead, which is the same bargain the rest of
//! this issue makes: `install sway-config` fails the install if a flag strip
//! did not take, and `headless.sh` dies if `sway.conf` stops passing
//! `--no-state-custodian`. A duplication that must exist gets something that
//! breaks loudly when it drifts.

use shepherd_state_proto::{STATE_USER, admin_dir, socket_path, state_dir};

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
    let listen = directive(&unit("shepherd-stated@.socket"), "ListenStream");

    // `%i` is systemd's instance name, which is the kiosk user — so the unit
    // and the constant agree exactly when substituting one gives the other.
    let expected = socket_path("%i");
    assert_eq!(
        std::path::Path::new(&listen),
        expected,
        "shepherd-stated@.socket listens on {listen}, but shepherd_state_proto::socket_path \
         dials {}. shepherdd would fall back to an unprotected store and report only that \
         the custodian could not be reached.",
        expected.display()
    );
}

#[test]
fn the_service_unit_owns_the_directory_the_client_stats() {
    let service = unit("shepherd-stated@.service");

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
        "shepherd-stated@.service does not create {}, where the admin record, the unbond \
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
        "shepherd-stated@.service keeps state in {}, but shepherd_state_proto::state_dir \
         looks in {}. shepherdd stats that path to tell a broken custodian apart from a \
         device that never had one, so a drift here makes a protected device look fresh — \
         and a fresh device is one the next phone may claim.",
        absolute.display(),
        state_dir("%i").display()
    );

    for key in ["User", "Group"] {
        assert_eq!(
            directive(&service, key),
            STATE_USER,
            "shepherd-stated@.service runs as a different {key} than \
             shepherd_state_proto::STATE_USER, which the client checks the socket's owner \
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

//! The installer's idea of shepherd's protected files must match this crate's.
//!
//! [`ProtectedFile`] is the Rust list; `scripts/lib/install.sh` carries a shell
//! list of the same files, because the migration that moves a device's state
//! under the custodian is a shell script and cannot ask Rust. Two lists is the
//! arrangement we are stuck with. Two lists that nothing compares is not.
//!
//! This is not hypothetical tidiness. `unbond-queue.toml` was added to
//! [`ProtectedFile`] and the installer was not told: the custodian wrote the
//! queue of BlueZ bonds still to be removed into its own directory, and neither
//! the migration nor `uninstall state --restore-to-home` moved it. A pending
//! unbond was stranded — leaving the old phone's bond in place, which is the
//! one thing a factory reset exists to prevent.

use shepherd_util::{FileScope, ProtectedFile};

/// Every variant, so adding one to the enum without adding it here is a
/// compile error rather than a silently narrower test.
const ALL: &[ProtectedFile] = &[
    ProtectedFile::Config,
    ProtectedFile::AdminRecord,
    ProtectedFile::ResetSentinel,
    ProtectedFile::UnbondQueue,
    ProtectedFile::WebAuth,
    ProtectedFile::TlsCert,
];

fn installer() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/lib/install.sh")
        .canonicalize()
        .expect("scripts/lib/install.sh is where the workspace keeps it");
    std::fs::read_to_string(path).expect("reading install.sh")
}

/// The names in a `NAME=(a b c)` array, or a `NAME="value"` scalar.
fn shell_names(script: &str, var: &str) -> Vec<String> {
    let line = script
        .lines()
        .find(|l| l.trim_start().starts_with(&format!("{var}=")))
        .unwrap_or_else(|| panic!("install.sh no longer defines {var}"));
    let value = line.split_once('=').expect("an assignment").1.trim();
    value
        .trim_start_matches('(')
        .trim_end_matches(')')
        .split_whitespace()
        .map(|name| name.trim_matches('"').to_string())
        .collect()
}

#[test]
fn every_protected_file_is_accounted_for_by_the_installer() {
    let script = installer();

    // Three fates, and every protected file has exactly one: moved with the
    // rest of the state, moved as the policy (a different source directory), or
    // deliberately left behind. The third is declared rather than omitted so
    // that "decided against" reads differently from "forgotten" — which is the
    // distinction this test exists to make.
    let mut accounted: Vec<String> = shell_names(&script, "SHEPHERD_MIGRATED_FILES");
    accounted.extend(shell_names(&script, "SHEPHERD_SYSTEM_FILES"));
    accounted.extend(shell_names(&script, "SHEPHERD_POLICY_FILE"));
    accounted.extend(shell_names(&script, "SHEPHERD_UNMIGRATED_FILES"));

    for file in ALL {
        let name = file.file_name();
        assert!(
            accounted.iter().any(|n| n == name),
            "{name} is a ProtectedFile the installer says nothing about.\n\
             Add it to SHEPHERD_MIGRATED_FILES (a user's) or SHEPHERD_SYSTEM_FILES \
             (the device's) in scripts/lib/install.sh so it is carried across, or to \
             SHEPHERD_UNMIGRATED_FILES if it should be left behind on purpose.\n\
             The installer currently accounts for: {accounted:?}"
        );
    }
}

#[test]
fn the_installer_does_not_carry_files_that_no_longer_exist() {
    // The other direction. A name removed from `ProtectedFile` but left in the
    // installer is harmless on a device — it moves a file nothing writes — but
    // it is a lie about what shepherd keeps, and the next person to read the
    // list would believe it.
    let script = installer();
    let known: Vec<&str> = ALL
        .iter()
        .map(|f| f.file_name())
        // Owned by the custodian and reached through `Store` rather than
        // `ProtectedFiles`, so it is not a `ProtectedFile` and never will be.
        .chain(std::iter::once("shepherdd.db"))
        .collect();

    for name in shell_names(&script, "SHEPHERD_MIGRATED_FILES")
        .into_iter()
        .chain(shell_names(&script, "SHEPHERD_SYSTEM_FILES"))
        .chain(shell_names(&script, "SHEPHERD_UNMIGRATED_FILES"))
    {
        assert!(
            known.contains(&name.as_str()),
            "install.sh migrates {name}, which is not a file shepherd keeps any more \
             (known: {known:?})"
        );
    }
}

#[test]
fn the_installer_puts_each_file_in_the_scope_rust_says_it_has() {
    // The lists are not just "everything is somewhere": which list a file is in
    // decides which *directory* the installer moves it to, and that has to
    // agree with `ProtectedFile::scope`, which decides which directory the
    // custodian serves it from. A file the installer treats as a user's and the
    // daemon reads as the device's is one the daemon never finds.
    let script = installer();
    let per_user = shell_names(&script, "SHEPHERD_MIGRATED_FILES");
    let system = shell_names(&script, "SHEPHERD_SYSTEM_FILES");
    let policy = shell_names(&script, "SHEPHERD_POLICY_FILE");

    for file in ALL {
        let name = file.file_name().to_string();
        // The sentinel is deliberately never carried across, so it appears in
        // neither list; its scope still matters to the daemon, which is checked
        // by the unit test on `scope()` itself rather than here.
        if file == &ProtectedFile::ResetSentinel {
            continue;
        }
        match file.scope() {
            FileScope::System => assert!(
                system.contains(&name),
                "{name} is a device file to Rust but the installer moves it to a user's \
                 directory (SHEPHERD_SYSTEM_FILES has {system:?})"
            ),
            FileScope::PerUser => assert!(
                per_user.contains(&name) || policy.contains(&name),
                "{name} is a user's file to Rust but the installer does not move it to \
                 their directory (SHEPHERD_MIGRATED_FILES has {per_user:?}, \
                 SHEPHERD_POLICY_FILE has {policy:?})"
            ),
        }
    }
}

//! Fail CI when the committed codegen outputs no longer match what
//! the current `ManagementService` trait would produce. Running
//! `cargo run -p shepherd-wire-codegen --bin rpc-codegen` should always
//! be idempotent; if this test starts failing, that's the fix.
//!
//! The alternative — regenerating on every build via `build.rs` —
//! would hide drift from PR review. Keeping the outputs checked in
//! and diffed on CI means every schema change shows up in the diff.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .expect("repo root above CARGO_MANIFEST_DIR")
        .to_path_buf()
}

#[test]
fn codegen_outputs_match_checked_in() {
    // Run the codegen binary into a temp directory rather than
    // touching the real repo files, then compare byte-for-byte.
    let scratch = tempfile::tempdir().expect("tempdir");
    let out_dir = scratch.path();

    let status = Command::new(env!("CARGO"))
        .args([
            "run",
            "--quiet",
            "-p",
            "shepherd-wire-codegen",
            "--bin",
            "rpc-codegen",
            "--",
        ])
        .env("SHEPHERD_RPC_CODEGEN_OUT", out_dir)
        .current_dir(repo_root())
        .status()
        .expect("cargo run rpc-codegen");
    assert!(status.success(), "rpc-codegen exited non-zero");

    let files = [
        ("docs/rpc-schema.json", "rpc-schema.json"),
        (
            "companion-android/app/src/main/kotlin/com/armeafamily/shepherd/companion/ble/RpcMethods.kt",
            "RpcMethods.kt",
        ),
        (
            "shepherd-webui/src/api/rpc-methods.generated.ts",
            "rpc-methods.generated.ts",
        ),
        // The payload types. This is the artifact that matters most: the
        // companion's hand-written mirrors drifted twice before they were
        // generated, and neither drift was catchable from the method schema.
        (
            "companion-android/app/src/main/kotlin/com/armeafamily/shepherd/companion/domain/WireTypes.generated.kt",
            "WireTypes.generated.kt",
        ),
        // The web UI's half of the same story. Its types were hand-written for
        // as long as the companion's were, with the same exposure and none of
        // the detection: `tsc` checks TypeScript against TypeScript and cannot
        // see the Rust shape at all.
        (
            "shepherd-webui/src/api/wire-types.generated.ts",
            "wire-types.generated.ts",
        ),
        // The config editor's mirrors of the `Raw*` types. Same reasoning: a
        // field added to `schema.rs` that the editor never renders is a field
        // nobody can set, and only a generated mirror makes that visible.
        (
            "shepherd-webui/src/config/model/config.generated.ts",
            "config.generated.ts",
        ),
    ];

    for (checked_in, temp_name) in files {
        let checked_in_path = repo_root().join(checked_in);
        let temp_path = out_dir.join(temp_name);
        let checked_in_contents =
            std::fs::read_to_string(&checked_in_path).expect("checked-in file present");
        let regenerated = std::fs::read_to_string(&temp_path).expect("codegen produced file");
        assert_eq!(
            checked_in_contents, regenerated,
            "{} drifted from what the current trait would produce; run\n  \
             cargo run -p shepherd-wire-codegen --bin rpc-codegen\n\
             from the repo root to regenerate.",
            checked_in
        );
    }
}

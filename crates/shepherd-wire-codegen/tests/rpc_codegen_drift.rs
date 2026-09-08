//! Fail CI when the committed codegen outputs no longer match what
//! the current `ManagementService` trait would produce. Running
//! `cargo run -p shepherd-wire-codegen --bin rpc-codegen` should always
//! be idempotent; if this test starts failing, that's the fix.
//!
//! The alternative — regenerating on every build via `build.rs` —
//! would hide drift from PR review. Keeping the outputs checked in
//! and diffed on CI means every schema change shows up in the diff.

// Re-runs the generator via `env!("CARGO")` — an absolute path the toolchain
// supplied, at build time, with no daemon and no kiosk user involved (#144).
#![allow(clippy::disallowed_methods)]

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
        // The request half of the protocol. Param *names* were the last part
        // of the wire contract still hand-written on both clients, where a
        // rename compiled on both sides and failed only when someone tapped
        // the button.
        (
            "companion-android/app/src/main/kotlin/com/armeafamily/shepherd/companion/domain/RpcParams.generated.kt",
            "RpcParams.generated.kt",
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
        // What the editor decodes back out of the wasm module. Both of these
        // were hand-written mirrors carrying a "Mirrors <rust file>" header —
        // a promise no test kept, and one the compiler could not help with:
        // adding a `ValidationError` variant forces the Rust `From` impl to
        // handle it and leaves the TypeScript untouched.
        (
            "shepherd-webui/src/config/model/wasm-types.generated.ts",
            "wasm-types.generated.ts",
        ),
        // The per-kind defaults. Not a type mirror but a *value* one: what an
        // entry gets for a field it leaves unset, which the editor has to show
        // before the daemon has resolved anything. Two of these were mirrored
        // by hand first (issues #78 and #160), which is how they got here.
        (
            "shepherd-webui/src/config/model/kind-defaults.generated.ts",
            "kind-defaults.generated.ts",
        ),
        // The per-*field* defaults, from both places the daemon keeps them:
        // serde's, which reach the JSON Schema on their own, and the ones
        // `Policy::from_raw` resolves at load time, which do not. About forty
        // of these were spelled out in the editor by hand — a `?? true`, a
        // `?? "kiosk"`, a `const DEFAULT_COOLDOWN_MIN_SESSION = 120` — with
        // nothing anywhere to notice when the Rust moved.
        (
            "shepherd-webui/src/config/model/field-defaults.generated.ts",
            "field-defaults.generated.ts",
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

/// The companion's `Protocol.kt` is hand-written — the GATT UUIDs and the
/// protocol version are a wire contract the codegen does not emit — so nothing
/// else notices when one side moves and the other does not.
///
/// Bumping `PROTOCOL_VERSION` on the device and not in the app compiles
/// cleanly on both sides and surfaces only as a phone that refuses to pair,
/// with a message blaming the app for being out of date. That is how it was
/// found during #149, on a phone that had just been given the new build.
#[test]
fn protocol_constants_match_the_companion() {
    let kotlin = std::fs::read_to_string(repo_root().join(
        "companion-android/app/src/main/kotlin/com/armeafamily/shepherd/companion/ble/Protocol.kt",
    ))
    .expect("companion Protocol.kt");

    let expected_version = format!(
        "const val PROTOCOL_VERSION: Long = {}",
        shepherd_ble::protocol::PROTOCOL_VERSION
    );
    assert!(
        kotlin.contains(&expected_version),
        "companion Protocol.kt does not declare `{expected_version}`; the device speaks \
         protocol v{} and the app has to agree",
        shepherd_ble::protocol::PROTOCOL_VERSION,
    );

    for (name, uuid) in [
        (
            "MANAGEMENT_SERVICE",
            shepherd_ble::protocol::SHEPHERD_MANAGEMENT_SERVICE_UUID,
        ),
        (
            "DEVICE_INFO_CHAR",
            shepherd_ble::protocol::SHEPHERD_DEVICE_INFO_CHAR_UUID,
        ),
        (
            "REQUEST_CHAR",
            shepherd_ble::protocol::SHEPHERD_REQUEST_CHAR_UUID,
        ),
        (
            "RESPONSE_CHAR",
            shepherd_ble::protocol::SHEPHERD_RESPONSE_CHAR_UUID,
        ),
        (
            "EVENTS_CHAR",
            shepherd_ble::protocol::SHEPHERD_EVENTS_CHAR_UUID,
        ),
    ] {
        let expected = format!("val {name}: Uuid = Uuid.parse(\"{uuid}\")");
        assert!(
            kotlin.contains(&expected),
            "companion Protocol.kt does not declare `{expected}`",
        );
    }
}

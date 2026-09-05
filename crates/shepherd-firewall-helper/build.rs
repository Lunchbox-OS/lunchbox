//! Build the sibling `shepherd-firewall-bpf` crate (target
//! `bpfel-unknown-none`, nightly toolchain) and emit
//! `SHEPHERD_FIREWALL_BPF_OBJ` so `main.rs` can `include_bytes!` the result.
//!
//! That crate is excluded from the workspace, so a plain `cargo build` for
//! the helper would otherwise leave us without a BPF object.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let helper_manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bpf_dir = helper_manifest
        .parent()
        .unwrap()
        .join("shepherd-firewall-bpf");

    println!("cargo:rerun-if-changed={}/src/main.rs", bpf_dir.display());
    println!("cargo:rerun-if-changed={}/Cargo.toml", bpf_dir.display());
    println!(
        "cargo:rerun-if-changed={}/.cargo/config.toml",
        bpf_dir.display()
    );

    // The BPF crate needs the nightly toolchain (for `-Zbuild-std=core`)
    // and the bpfel-unknown-none target (which has no precompiled
    // libcore). Pin the toolchain explicitly via `rustup run nightly`:
    // - `rust-toolchain.toml` in the BPF crate is ignored because the
    //   rustup proxy detection is bypassed inside a build script.
    // - The parent cargo leaks RUSTUP_TOOLCHAIN/CARGO/RUSTFLAGS that pin
    //   the host toolchain; strip them.
    // - CARGO_TARGET_DIR would redirect target/ outside the BPF crate;
    //   strip it so artifacts land where build.rs expects.
    // - CARGO_BUILD_TARGET would beat the BPF crate's own
    //   .cargo/config.toml `[build] target = "bpfel-unknown-none"` (env wins
    //   over config) and try to compile the eBPF program for whatever the
    //   parent build is targeting. Nothing here sets it -- a cross build
    //   passes `--target` on the command line for exactly this reason -- but
    //   an exported one from anywhere else must not reach this child.
    // A build script, not a daemon: this runs on a developer's machine or a
    // CI runner at compile time, where `$PATH` is the toolchain's own and there
    // is no kiosk user to have chosen it (issue #144).
    #[allow(clippy::disallowed_methods)]
    let status = Command::new("rustup")
        .args(["run", "nightly", "cargo", "build", "--release"])
        .current_dir(&bpf_dir)
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("CARGO")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_TARGET")
        .env_remove("CARGO_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .env_remove("RUSTC")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("RUSTDOC")
        .status()
        .expect("spawn `rustup run nightly cargo` for shepherd-firewall-bpf");
    assert!(status.success(), "shepherd-firewall-bpf build failed");

    let bpf_obj = bpf_dir.join("target/bpfel-unknown-none/release/shepherd-firewall-bpf");
    assert!(
        bpf_obj.exists(),
        "BPF object not produced at {}",
        bpf_obj.display()
    );

    println!(
        "cargo:rustc-env=SHEPHERD_FIREWALL_BPF_OBJ={}",
        bpf_obj.display()
    );
}

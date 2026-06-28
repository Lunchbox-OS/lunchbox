//! Privileged helper for the Android (Waydroid) activity kind.
//!
//! shepherdd runs unprivileged but two Waydroid operations need root. This
//! tiny helper is invoked via `pkexec` (gated by polkit — see
//! `dist/polkit/org.shepherd.waydroid.policy` and `50-shepherd-waydroid.rules`)
//! and exposes exactly two narrow, fixed actions:
//!
//! - `force-stop --package <pkg>`: `waydroid shell am force-stop <pkg>` to
//!   reclaim the cached Android process after its window is closed. The package
//!   name is re-validated here (the trust boundary) with the same shared rule
//!   as config, then passed as a single argv element — no shell.
//! - `preboot`: `systemctl start waydroid-container` to bring the (root) LXC
//!   container service up so shepherdd can then start the user-level session.
//!   Takes no arguments; the unit name is hardcoded, so the action cannot be
//!   pointed at any other service.
//!
//! Both subcommands `exec()` a fixed command with a fixed argv. The helper does
//! not read config, env, or any caller-controlled data beyond the validated
//! package name. See README.md for the trust boundary.

use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

use shepherd_util::is_valid_android_package;

const HELPER_NAME: &str = "shepherd-waydroid-helper";

/// The one systemd unit `preboot` is allowed to start. Hardcoded so the
/// privileged action can never be aimed at another service.
const CONTAINER_UNIT: &str = "waydroid-container";

fn die(msg: impl AsRef<str>) -> ! {
    eprintln!("{HELPER_NAME}: {}", msg.as_ref());
    std::process::exit(2);
}

/// The fixed StatusBarManager disable flags `lock-down` applies. These block the
/// child's routes out of the kiosk app: the notification shade / quick settings
/// (which can reach Android Settings), notification peeking, and the nav-bar
/// home/recents/search buttons. Hardcoded — no caller input.
const LOCK_DOWN_FLAGS: &[&str] = &[
    "home",
    "recents",
    "statusbar-expansion",
    "notification-peek",
    "search",
];

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("force-stop") => force_stop(args),
        Some("preboot") => preboot(args),
        Some("lock-down") => lock_down(args),
        Some(other) => die(format!(
            "unknown subcommand '{other}' (expected 'force-stop', 'preboot', or 'lock-down')"
        )),
        None => die("missing subcommand (expected 'force-stop', 'preboot', or 'lock-down')"),
    }
}

/// `force-stop --package <pkg>` → `waydroid shell am force-stop <pkg>`.
fn force_stop(mut args: impl Iterator<Item = String>) -> ExitCode {
    let mut package: Option<String> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--package" => {
                package = Some(
                    args.next()
                        .unwrap_or_else(|| die("--package needs a value")),
                );
            }
            other => die(format!("unexpected argument '{other}'")),
        }
    }
    let package = package.unwrap_or_else(|| die("--package is required"));

    // The trust boundary: never run `am force-stop` on an unvalidated string.
    // The strict rule forbids leading '-', whitespace, and shell metacharacters,
    // so passing it as a single argv element below is injection-safe.
    if !is_valid_android_package(&package) {
        die(format!("invalid package name '{package}'"));
    }

    // Fixed argv, no shell. exec() replaces this process; it only returns on
    // failure to launch `waydroid`.
    let err = Command::new("waydroid")
        .args(["shell", "am", "force-stop"])
        .arg(&package)
        .exec();
    die(format!("failed to exec waydroid: {err}"));
}

/// `preboot` → `systemctl start waydroid-container` (idempotent).
fn preboot(mut args: impl Iterator<Item = String>) -> ExitCode {
    if let Some(extra) = args.next() {
        die(format!("'preboot' takes no arguments, got '{extra}'"));
    }
    let err = Command::new("systemctl")
        .args(["start", CONTAINER_UNIT])
        .exec();
    die(format!("failed to exec systemctl: {err}"));
}

/// `lock-down` → `waydroid shell cmd statusbar send-disable-flag <flags>`.
/// Hardens the running session against the child leaving the kiosk app. Flags
/// are a fixed compile-time set; takes no arguments.
fn lock_down(mut args: impl Iterator<Item = String>) -> ExitCode {
    if let Some(extra) = args.next() {
        die(format!("'lock-down' takes no arguments, got '{extra}'"));
    }
    let err = Command::new("waydroid")
        .args(["shell", "cmd", "statusbar", "send-disable-flag"])
        .args(LOCK_DOWN_FLAGS)
        .exec();
    die(format!("failed to exec waydroid: {err}"));
}

//! Privileged helper for the Android (Waydroid) activity kind.
//!
//! shepherdd runs unprivileged but a few Waydroid operations need root. This
//! tiny helper is invoked via `pkexec` (gated by polkit — see
//! `dist/polkit/org.shepherd.waydroid.policy` and `50-shepherd-waydroid.rules`)
//! and exposes exactly five narrow, fixed actions:
//!
//! - `force-stop --package <pkg>`: `waydroid shell am force-stop <pkg>` to
//!   reclaim the cached Android process after its window is closed. The package
//!   name is re-validated here (the trust boundary) with the same shared rule
//!   as config, then passed as a single argv element — no shell.
//! - `preboot`: `systemctl start waydroid-container` to bring the (root) LXC
//!   container service up so shepherdd can then start the user-level session.
//! - `lock-down`: `waydroid shell cmd statusbar send-disable-flag <flags>` to
//!   harden the session against the child leaving the kiosk app.
//! - `pin --package <pkg>`: drive the DPC's `LaunchActivity` to launch `<pkg>`
//!   pinned in Lock Task Mode (`lock_mode = "locktask"`). Same package trust
//!   boundary as force-stop.
//! - `unlock`: broadcast to the DPC's `ControlReceiver` to clear the Lock Task
//!   allowlist so a locked session can end.
//!
//! The no-argument actions take no arguments and every action `exec`s a fixed
//! command (the only caller-controlled value is the validated package name).
//! Parsing/validation is split into the pure [`parse_args`] + [`Action`] so the
//! trust boundary is unit-tested without exec. See README.md.

use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

use shepherd_util::is_valid_android_package;

const HELPER_NAME: &str = "shepherd-waydroid-helper";

/// The one systemd unit `preboot` is allowed to start. Hardcoded so the
/// privileged action can never be aimed at another service.
const CONTAINER_UNIT: &str = "waydroid-container";

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

/// The DPC (Device Policy Controller) components `pin`/`unlock` drive. Hardcoded
/// so the privileged action targets only shepherd's own device-owner app.
const DPC_LAUNCH_COMPONENT: &str = "com.armeafamily.shepherd.dpc/.LaunchActivity";
const DPC_CONTROL_COMPONENT: &str = "com.armeafamily.shepherd.dpc/.ControlReceiver";

const USAGE: &str = "expected 'force-stop', 'preboot', 'lock-down', 'pin', or 'unlock'";

/// A validated, ready-to-exec privileged action.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    ForceStop { package: String },
    Preboot,
    LockDown,
    Pin { package: String },
    Unlock,
}

impl Action {
    /// The exact `(program, argv)` this action runs. Fixed and shell-free; the
    /// only variable is the already-validated package name.
    fn command(&self) -> (&'static str, Vec<String>) {
        match self {
            Action::ForceStop { package } => (
                "waydroid",
                vec![
                    "shell".into(),
                    "am".into(),
                    "force-stop".into(),
                    package.clone(),
                ],
            ),
            Action::Preboot => ("systemctl", vec!["start".into(), CONTAINER_UNIT.into()]),
            Action::LockDown => {
                let mut argv = vec![
                    "shell".into(),
                    "cmd".into(),
                    "statusbar".into(),
                    "send-disable-flag".into(),
                ];
                argv.extend(LOCK_DOWN_FLAGS.iter().map(|f| f.to_string()));
                ("waydroid", argv)
            }
            // `shell --` stops waydroid from parsing the forwarded `--es` flags
            // as its own; the component is fixed and the package pre-validated.
            Action::Pin { package } => (
                "waydroid",
                vec![
                    "shell".into(),
                    "--".into(),
                    "am".into(),
                    "start".into(),
                    "-n".into(),
                    DPC_LAUNCH_COMPONENT.into(),
                    "--es".into(),
                    "pkg".into(),
                    package.clone(),
                ],
            ),
            Action::Unlock => (
                "waydroid",
                vec![
                    "shell".into(),
                    "--".into(),
                    "am".into(),
                    "broadcast".into(),
                    "-n".into(),
                    DPC_CONTROL_COMPONENT.into(),
                    "--es".into(),
                    "action".into(),
                    "unlock".into(),
                ],
            ),
        }
    }
}

fn die(msg: impl AsRef<str>) -> ! {
    eprintln!("{HELPER_NAME}: {}", msg.as_ref());
    std::process::exit(2);
}

/// Parse argv (after the program name) into an [`Action`], or an error message.
/// Pure — no exec, no process exit — so the dispatch and the package-name trust
/// boundary are unit-testable.
fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Action, String> {
    match args.next().as_deref() {
        Some("force-stop") => {
            parse_validated_package(args).map(|package| Action::ForceStop { package })
        }
        Some("preboot") => no_extra_args(args, "preboot").map(|()| Action::Preboot),
        Some("lock-down") => no_extra_args(args, "lock-down").map(|()| Action::LockDown),
        Some("pin") => parse_validated_package(args).map(|package| Action::Pin { package }),
        Some("unlock") => no_extra_args(args, "unlock").map(|()| Action::Unlock),
        Some(other) => Err(format!("unknown subcommand '{other}' ({USAGE})")),
        None => Err(format!("missing subcommand ({USAGE})")),
    }
}

/// Parse `--package <pkg>` and enforce the shared trust boundary — the only
/// caller-controlled value in any `waydroid shell` action. The strict rule
/// forbids leading '-', whitespace, and shell metacharacters, so passing it as a
/// single argv element is injection-safe. Shared by `force-stop` and `pin`.
fn parse_validated_package(mut args: impl Iterator<Item = String>) -> Result<String, String> {
    let mut package: Option<String> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--package" => {
                package = Some(
                    args.next()
                        .ok_or_else(|| "--package needs a value".to_string())?,
                );
            }
            other => return Err(format!("unexpected argument '{other}'")),
        }
    }
    let package = package.ok_or_else(|| "--package is required".to_string())?;
    if !is_valid_android_package(&package) {
        return Err(format!("invalid package name '{package}'"));
    }
    Ok(package)
}

fn no_extra_args(mut args: impl Iterator<Item = String>, name: &str) -> Result<(), String> {
    match args.next() {
        Some(extra) => Err(format!("'{name}' takes no arguments, got '{extra}'")),
        None => Ok(()),
    }
}

fn main() -> ExitCode {
    let action = parse_args(std::env::args().skip(1)).unwrap_or_else(|e| die(e));
    let (program, args) = action.command();
    // exec() replaces this process; it only returns on failure to launch.
    let err = Command::new(program).args(&args).exec();
    die(format!("failed to exec {program}: {err}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Action, String> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn force_stop_accepts_valid_package() {
        assert_eq!(
            parse(&["force-stop", "--package", "com.android.calculator2"]),
            Ok(Action::ForceStop {
                package: "com.android.calculator2".into()
            })
        );
    }

    #[test]
    fn force_stop_rejects_unsafe_package() {
        // Same trust boundary as config — shell metacharacters, leading dash,
        // and single-segment names are all rejected before any exec.
        assert!(parse(&["force-stop", "--package", "com.app;rm -rf"]).is_err());
        assert!(parse(&["force-stop", "--package", "-rf"]).is_err());
        assert!(parse(&["force-stop", "--package", "noseparator"]).is_err());
        assert!(parse(&["force-stop", "--package", "com..app"]).is_err());
    }

    #[test]
    fn force_stop_requires_package_with_value() {
        assert!(parse(&["force-stop"]).is_err());
        assert!(parse(&["force-stop", "--package"]).is_err());
    }

    #[test]
    fn force_stop_rejects_extra_arguments() {
        assert!(parse(&["force-stop", "--package", "a.b", "extra"]).is_err());
        assert!(parse(&["force-stop", "--bogus"]).is_err());
    }

    #[test]
    fn preboot_and_lock_down_take_no_args() {
        assert_eq!(parse(&["preboot"]), Ok(Action::Preboot));
        assert_eq!(parse(&["lock-down"]), Ok(Action::LockDown));
        assert!(parse(&["preboot", "x"]).is_err());
        assert!(parse(&["lock-down", "x"]).is_err());
    }

    #[test]
    fn unknown_or_missing_subcommand_is_error() {
        assert!(parse(&["frobnicate"]).is_err());
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn pin_shares_the_force_stop_package_trust_boundary() {
        assert_eq!(
            parse(&["pin", "--package", "com.android.calculator2"]),
            Ok(Action::Pin {
                package: "com.android.calculator2".into()
            })
        );
        assert!(parse(&["pin", "--package", "com.app;rm -rf"]).is_err());
        assert!(parse(&["pin", "--package", "-rf"]).is_err());
        assert!(parse(&["pin", "--package", "noseparator"]).is_err());
        assert!(parse(&["pin"]).is_err());
        assert!(parse(&["pin", "--package", "a.b", "extra"]).is_err());
    }

    #[test]
    fn unlock_takes_no_args() {
        assert_eq!(parse(&["unlock"]), Ok(Action::Unlock));
        assert!(parse(&["unlock", "x"]).is_err());
    }

    /// argv as `&str`s, for ergonomic comparison.
    fn cmd_of(action: Action) -> (&'static str, Vec<String>) {
        action.command()
    }

    #[test]
    fn commands_are_fixed_and_shell_free() {
        let (prog, argv) = cmd_of(Action::Preboot);
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        assert_eq!(prog, "systemctl");
        assert_eq!(argv, ["start", "waydroid-container"]);

        let (prog, argv) = cmd_of(Action::ForceStop {
            package: "com.x.y".into(),
        });
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        assert_eq!(prog, "waydroid");
        assert_eq!(argv, ["shell", "am", "force-stop", "com.x.y"]);

        let (prog, argv) = cmd_of(Action::LockDown);
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        assert_eq!(prog, "waydroid");
        assert_eq!(
            argv,
            [
                "shell",
                "cmd",
                "statusbar",
                "send-disable-flag",
                "home",
                "recents",
                "statusbar-expansion",
                "notification-peek",
                "search",
            ]
        );

        let (prog, argv) = cmd_of(Action::Pin {
            package: "com.x.y".into(),
        });
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        assert_eq!(prog, "waydroid");
        assert_eq!(
            argv,
            [
                "shell",
                "--",
                "am",
                "start",
                "-n",
                "com.armeafamily.shepherd.dpc/.LaunchActivity",
                "--es",
                "pkg",
                "com.x.y",
            ]
        );

        let (prog, argv) = cmd_of(Action::Unlock);
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        assert_eq!(prog, "waydroid");
        assert_eq!(
            argv,
            [
                "shell",
                "--",
                "am",
                "broadcast",
                "-n",
                "com.armeafamily.shepherd.dpc/.ControlReceiver",
                "--es",
                "action",
                "unlock",
            ]
        );
    }
}

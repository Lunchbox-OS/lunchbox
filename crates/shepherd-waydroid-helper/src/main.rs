//! Privileged helper for the Android (Waydroid) activity kind.
//!
//! shepherdd runs unprivileged but a few Waydroid operations need root. This
//! tiny helper is invoked via `pkexec` (gated by polkit — see
//! `dist/polkit/org.shepherd.waydroid.policy` and `50-shepherd-waydroid.rules`)
//! and exposes exactly eleven narrow, fixed actions:
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
//! - `boot-completed`: exit 0 iff `waydroid shell getprop sys.boot_completed`
//!   prints `1` — Android inside the running session has finished booting. The
//!   readiness gate polls this to keep Android activities hidden until a launch
//!   would land on a booted system instead of the boot animation. Alone among
//!   the actions it is a *query* that inspects the command's output (getprop
//!   always exits 0), so it does not `exec` — see [`main`].
//! - `maximize --package <pkg>`: grow the foreground app's freeform window to
//!   fill the display (`am task resize`), so a multi-window Android app fills
//!   its host window instead of Waydroid's small default freeform size. Like
//!   `boot-completed` it is multi-step (read the top task + display size, verify
//!   `<pkg>` is on top, then resize), so it does not `exec` — see [`main`].
//! - `back`: `waydroid shell input keyevent 4` (Android `KEYCODE_BACK`) to the
//!   foreground app. Backs the HUD's back button (`lock_mode = "statusbar"`
//!   fullscreens the app under the HUD, hiding Android's own caption back). The
//!   key is dispatched to the *input-focused* window, which Waydroid only sets
//!   once the app has been interacted with — fine in practice (the child is
//!   using the app), but a just-launched, untouched app has no focus yet.
//! - `max-volume`: pin Android's media stream (STREAM_MUSIC) to max via
//!   `cmd media_session volume`. Android's per-stream volume pre-attenuates
//!   playback before the host sink shepherd controls, so its mid-range default
//!   caps loudness; maxing it hands the full range to shepherd. `--set` rejects an
//!   out-of-range index and the max is ROM-specific, so it is multi-step (read the
//!   max from `--get`, then `--set` it) and does not `exec` — see [`main`].
//! - `scale-density <permille>`: set Android's UI density to `permille`/1000 of the
//!   panel's base density (1500 = 1.5x), so a fractional-scale kiosk gets a larger
//!   Android UI. Waydroid can't handle a fractional wl_output scale (its buffer is
//!   boot-locked), so shepherd runs it at native scale 1 and carries the zoom as
//!   density instead. Multi-step (read base density, then set), so no `exec`.
//! - `is-running --package <pkg>`: exit 0 iff `<pkg>` has a live Android process
//!   (`waydroid shell pidof <pkg>` prints a pid). The pre-launch guard polls this
//!   so a fast reopen waits for the previous instance to finish dying rather than
//!   racing its teardown (which wedges the platform bridge). A *query* that
//!   inspects output (pidof's exit code isn't reliable through `waydroid shell`),
//!   so it does not `exec` — see [`main`].
//!
//! Every action but `boot-completed`/`maximize`/`max-volume`/`scale-density`/
//! `is-running` `exec`s a fixed command (the only caller-controlled values are the
//! validated package name and the bounded permille); those five instead run fixed,
//! shell-free commands and read their output rather than replacing the process.
//! Parsing and validation are split into the pure [`parse_args`] + [`Action`] so
//! the trust boundary is unit-tested without exec. See README.md.

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

const USAGE: &str = "expected 'force-stop', 'preboot', 'lock-down', 'pin', 'unlock', \
     'boot-completed', 'maximize', 'back', 'max-volume', 'scale-density', or 'is-running'";

/// A validated, ready-to-exec privileged action.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    ForceStop {
        package: String,
    },
    Preboot,
    LockDown,
    Pin {
        package: String,
    },
    Unlock,
    /// Query: exit 0 iff Android has finished booting. Inspects output, so
    /// [`main`] handles it specially instead of `exec`ing.
    BootCompleted,
    /// Grow `package`'s foreground freeform window to fill the display. Multi-step
    /// (read state, then resize), so [`main`] handles it specially, not `exec`.
    Maximize {
        package: String,
    },
    /// Send Android `KEYCODE_BACK` to the foreground app (the HUD back button).
    Back,
    /// Pin Android's media stream to max so shepherd's host volume owns the full
    /// dynamic range instead of it being pre-attenuated inside Android.
    MaxVolume,
    /// Scale Android's UI density to `permille`/1000 of the panel's base density,
    /// so a fractional-scale kiosk (e.g. `output * scale 1.5`) gets a proportionally
    /// larger Android UI (Waydroid renders at native scale 1; density carries the
    /// zoom). Multi-step (read base density, then set), so [`main`] handles it
    /// specially, not `exec`.
    ScaleDensity {
        permille: u32,
    },
    /// Query: exit 0 iff `package` has a live Android process. Inspects output
    /// (pidof), so [`main`] handles it specially, not `exec`.
    IsRunning {
        package: String,
    },
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
            Action::BootCompleted => (
                "waydroid",
                vec![
                    "shell".into(),
                    "getprop".into(),
                    "sys.boot_completed".into(),
                ],
            ),
            // The state read that `maximize` starts from; it then parses the top
            // task + display size and issues a second `am task resize` command.
            Action::Maximize { .. } => (
                "waydroid",
                vec![
                    "shell".into(),
                    "dumpsys".into(),
                    "activity".into(),
                    "activities".into(),
                ],
            ),
            // KEYCODE_BACK (4) to the input-focused foreground app.
            Action::Back => (
                "waydroid",
                vec![
                    "shell".into(),
                    "input".into(),
                    "keyevent".into(),
                    "4".into(),
                ],
            ),
            // The volume *read* max-volume starts from: STREAM_MUSIC (3) is the
            // media stream playback uses. `--set INDEX` rejects an out-of-range
            // index (no clamping) and the max is ROM-specific, so `main` parses it
            // from `--get`'s `[0..N]` and issues a second `--set N`. `shell --`
            // keeps waydroid from eating the forwarded `--stream`/`--get` flags.
            Action::MaxVolume => (
                "waydroid",
                vec![
                    "shell".into(),
                    "--".into(),
                    "cmd".into(),
                    "media_session".into(),
                    "volume".into(),
                    "--stream".into(),
                    "3".into(),
                    "--get".into(),
                ],
            ),
            // The base-density read scale-density starts from; `main` parses the
            // "Physical density: N" line and issues a second `wm density <scaled>`.
            Action::ScaleDensity { .. } => (
                "waydroid",
                vec!["shell".into(), "wm".into(), "density".into()],
            ),
            // pidof <pkg> — `main` reads its output to decide the exit code.
            Action::IsRunning { package } => (
                "waydroid",
                vec!["shell".into(), "pidof".into(), package.clone()],
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
        Some("boot-completed") => {
            no_extra_args(args, "boot-completed").map(|()| Action::BootCompleted)
        }
        Some("maximize") => {
            parse_validated_package(args).map(|package| Action::Maximize { package })
        }
        Some("back") => no_extra_args(args, "back").map(|()| Action::Back),
        Some("max-volume") => no_extra_args(args, "max-volume").map(|()| Action::MaxVolume),
        Some("scale-density") => {
            parse_permille(args).map(|permille| Action::ScaleDensity { permille })
        }
        Some("is-running") => {
            parse_validated_package(args).map(|package| Action::IsRunning { package })
        }
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

/// Parse the single `<permille>` argument of `scale-density`: the target density
/// as thousandths of the base (1500 = 1.5x). A plain positive integer, bounded to
/// a sane range so a typo can't drive Android to an unusable density.
fn parse_permille(mut args: impl Iterator<Item = String>) -> Result<u32, String> {
    let raw = args
        .next()
        .ok_or_else(|| "scale-density needs a <permille> value".to_string())?;
    if let Some(extra) = args.next() {
        return Err(format!("unexpected argument '{extra}'"));
    }
    let permille: u32 = raw
        .parse()
        .map_err(|_| format!("invalid permille '{raw}' (expected a positive integer)"))?;
    if !(500..=4000).contains(&permille) {
        return Err(format!("permille {permille} out of range [500, 4000]"));
    }
    Ok(permille)
}

fn main() -> ExitCode {
    let action = parse_args(std::env::args().skip(1)).unwrap_or_else(|e| die(e));
    // These are multi-step / output-inspecting rather than a single
    // fire-and-forget command, so they can't use the exec() path below.
    match &action {
        Action::BootCompleted => return boot_completed_exit_code(&action),
        Action::Maximize { package } => return maximize_exit_code(&action, package),
        Action::MaxVolume => return max_volume_exit_code(&action),
        Action::ScaleDensity { permille } => return scale_density_exit_code(&action, *permille),
        Action::IsRunning { .. } => return is_running_exit_code(&action),
        _ => {}
    }
    let (program, args) = action.command();
    // exec() replaces this process; it only returns on failure to launch.
    let err = Command::new(program).args(&args).exec();
    die(format!("failed to exec {program}: {err}"));
}

/// Run the fixed `boot-completed` command and map its output to an exit code:
/// success iff it prints `1` (Android finished booting). Any failure to run it
/// (session down, waydroid missing) or any other value is a non-success exit, so
/// the caller treats Android as not-yet-ready.
fn boot_completed_exit_code(action: &Action) -> ExitCode {
    let (program, args) = action.command();
    match Command::new(program).args(&args).output() {
        Ok(out) if String::from_utf8_lossy(&out.stdout).trim() == "1" => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

/// Run `pidof <pkg>` and map its output to an exit code: success iff it printed
/// a pid (the package has a live process). `pidof`'s own exit code isn't reliable
/// through `waydroid shell`, so inspect stdout. Any run failure is a non-success
/// exit — the caller treats the app as not running.
fn is_running_exit_code(action: &Action) -> ExitCode {
    let (program, args) = action.command();
    match Command::new(program).args(&args).output() {
        Ok(out) if !String::from_utf8_lossy(&out.stdout).trim().is_empty() => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

/// Grow the foreground app's freeform window to fill the display: read the
/// activity state, confirm `package` is the resumed app (so we never resize an
/// unrelated task — e.g. the app already closed), find the top task and the
/// display size, then `am task resize` the task to the full display. Any
/// failure is a non-success exit; the caller treats it as best-effort.
///
/// `action.command()` is the activity-state read; the resize is a second fixed
/// command whose only variable parts are integers parsed here.
fn maximize_exit_code(action: &Action, package: &str) -> ExitCode {
    let (program, args) = action.command();
    let dump = match Command::new(program).args(&args).output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
        Err(_) => return ExitCode::FAILURE,
    };
    // Only resize when the requested package is actually on top.
    let on_top = dump
        .lines()
        .any(|l| l.contains("ResumedActivity") && l.contains(&format!("{package}/")));
    if !on_top {
        return ExitCode::FAILURE;
    }
    let Some(task) = parse_top_task_id(&dump) else {
        return ExitCode::FAILURE;
    };
    let size = Command::new("waydroid")
        .args(["shell", "wm", "size"])
        .output();
    let Some((w, h)) = size
        .ok()
        .and_then(|o| parse_wm_size(&String::from_utf8_lossy(&o.stdout)))
    else {
        return ExitCode::FAILURE;
    };
    // task/w/h are all integers parsed above, so this argv is injection-safe.
    match Command::new("waydroid")
        .args([
            "shell",
            "am",
            "task",
            "resize",
            &task.to_string(),
            "0",
            "0",
            &w.to_string(),
            &h.to_string(),
        ])
        .status()
    {
        Ok(s) if s.success() => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

/// Pin Android's media stream to its max: read the current volume (whose output
/// carries the valid range), parse the max, then `--set` it. `--set` rejects an
/// out-of-range index rather than clamping and the max is ROM-specific, so a
/// fixed value can't be used. Any failure is a non-success exit (best-effort).
///
/// `action.command()` is the `--get` read; the `--set` is a second fixed command
/// whose only variable part is the integer max parsed here.
fn max_volume_exit_code(action: &Action) -> ExitCode {
    let (program, args) = action.command();
    let out = match Command::new(program).args(&args).output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
        Err(_) => return ExitCode::FAILURE,
    };
    let Some(max) = parse_volume_max(&out) else {
        return ExitCode::FAILURE;
    };
    // `max` is an integer parsed above, so this argv is injection-safe.
    match Command::new("waydroid")
        .args([
            "shell",
            "--",
            "cmd",
            "media_session",
            "volume",
            "--stream",
            "3",
            "--set",
            &max.to_string(),
        ])
        .status()
    {
        Ok(s) if s.success() => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

/// Parse the max volume index from `media_session volume --get` output, whose key
/// line reads `volume is <cur> in range [0..<max>]`.
fn parse_volume_max(out: &str) -> Option<u32> {
    let rest = out.split("in range [0..").nth(1)?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Scale Android's UI density to `permille`/1000 of the panel's *base* (physical)
/// density: read `wm density`'s "Physical density: N" (the base, unaffected by any
/// prior override), compute `N * permille / 1000`, and set it as the override. Any
/// failure is a non-success exit (best-effort).
///
/// `action.command()` is the `wm density` read; the set is a second fixed command
/// whose only variable part is the integer density computed here. Reading the
/// *physical* line keeps this idempotent — re-running never compounds an override.
fn scale_density_exit_code(action: &Action, permille: u32) -> ExitCode {
    let (program, args) = action.command();
    let out = match Command::new(program).args(&args).output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
        Err(_) => return ExitCode::FAILURE,
    };
    let Some(base) = parse_physical_density(&out) else {
        return ExitCode::FAILURE;
    };
    // permille is bounded in parse_permille and base comes from the device, so the
    // product can't overflow u32; the result is an integer, so the argv is safe.
    let target = (u64::from(base) * u64::from(permille) / 1000) as u32;
    if target == 0 {
        return ExitCode::FAILURE;
    }
    match Command::new("waydroid")
        .args(["shell", "wm", "density", &target.to_string()])
        .status()
    {
        Ok(s) if s.success() => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

/// Parse the base density from `wm density` output, whose first line reads
/// `Physical density: N`. Deliberately ignores any `Override density:` line so the
/// scaling is always relative to the panel's true base.
fn parse_physical_density(out: &str) -> Option<u32> {
    out.lines().find_map(|l| {
        l.trim()
            .strip_prefix("Physical density:")?
            .trim()
            .parse()
            .ok()
    })
}

/// Parse the top task id for user 0 (`mCurTaskIdForUser={0=<id>}`) from a
/// `dumpsys activity activities` dump — the task of the just-launched app.
fn parse_top_task_id(dump: &str) -> Option<u32> {
    let rest = dump.split("mCurTaskIdForUser={0=").nth(1)?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Parse the display size from `wm size` output (`Physical size: WxH`, or
/// `Override size: WxH` when an explicit override is set — that one wins).
fn parse_wm_size(out: &str) -> Option<(u32, u32)> {
    let pick = |prefix: &str| {
        out.lines().find_map(|l| {
            let (w, h) = l.trim().strip_prefix(prefix)?.trim().split_once('x')?;
            Some((w.trim().parse::<u32>().ok()?, h.trim().parse::<u32>().ok()?))
        })
    };
    pick("Override size:").or_else(|| pick("Physical size:"))
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

    #[test]
    fn boot_completed_takes_no_args() {
        assert_eq!(parse(&["boot-completed"]), Ok(Action::BootCompleted));
        assert!(parse(&["boot-completed", "x"]).is_err());
    }

    #[test]
    fn maximize_shares_the_force_stop_package_trust_boundary() {
        assert_eq!(
            parse(&["maximize", "--package", "com.android.calculator2"]),
            Ok(Action::Maximize {
                package: "com.android.calculator2".into()
            })
        );
        assert!(parse(&["maximize", "--package", "com.app;rm -rf"]).is_err());
        assert!(parse(&["maximize", "--package", "-rf"]).is_err());
        assert!(parse(&["maximize", "--package", "noseparator"]).is_err());
        assert!(parse(&["maximize"]).is_err());
        assert!(parse(&["maximize", "--package", "a.b", "extra"]).is_err());
    }

    #[test]
    fn back_takes_no_args() {
        assert_eq!(parse(&["back"]), Ok(Action::Back));
        assert!(parse(&["back", "x"]).is_err());
    }

    #[test]
    fn max_volume_takes_no_args() {
        assert_eq!(parse(&["max-volume"]), Ok(Action::MaxVolume));
        assert!(parse(&["max-volume", "x"]).is_err());
    }

    #[test]
    fn scale_density_parses_bounded_permille() {
        assert_eq!(
            parse(&["scale-density", "1500"]),
            Ok(Action::ScaleDensity { permille: 1500 })
        );
        assert_eq!(
            parse(&["scale-density", "1000"]),
            Ok(Action::ScaleDensity { permille: 1000 })
        );
        assert!(parse(&["scale-density"]).is_err()); // missing value
        assert!(parse(&["scale-density", "1500", "x"]).is_err()); // extra arg
        assert!(parse(&["scale-density", "abc"]).is_err()); // non-numeric
        assert!(parse(&["scale-density", "-5"]).is_err()); // negative
        assert!(parse(&["scale-density", "100"]).is_err()); // below range
        assert!(parse(&["scale-density", "9000"]).is_err()); // above range
    }

    #[test]
    fn parses_physical_density() {
        assert_eq!(
            parse_physical_density("Physical density: 180\nOverride density: 270\n"),
            Some(180)
        );
        assert_eq!(parse_physical_density("Physical density: 240"), Some(240));
        assert_eq!(parse_physical_density("garbage"), None);
    }

    #[test]
    fn is_running_shares_the_force_stop_package_trust_boundary() {
        assert_eq!(
            parse(&["is-running", "--package", "com.android.calculator2"]),
            Ok(Action::IsRunning {
                package: "com.android.calculator2".into()
            })
        );
        assert!(parse(&["is-running", "--package", "com.app;rm -rf"]).is_err());
        assert!(parse(&["is-running", "--package", "-rf"]).is_err());
        assert!(parse(&["is-running"]).is_err());
    }

    #[test]
    fn parses_volume_max() {
        let out = "[V] will control stream=3 (STREAM_MUSIC)\n[V] volume is 4 in range [0..15]\n";
        assert_eq!(parse_volume_max(out), Some(15));
        assert_eq!(
            parse_volume_max("[V] volume is 7 in range [0..25]"),
            Some(25)
        );
        assert_eq!(parse_volume_max("no range here"), None);
    }

    #[test]
    fn parses_top_task_id() {
        let dump = "  mFocusedApp=...\n  mCurTaskIdForUser={0=13}\n  more=stuff\n";
        assert_eq!(parse_top_task_id(dump), Some(13));
        assert_eq!(parse_top_task_id("no task here"), None);
    }

    #[test]
    fn parses_wm_size() {
        assert_eq!(
            parse_wm_size("Physical size: 1920x999\n"),
            Some((1920, 999))
        );
        // Override wins when both are present.
        assert_eq!(
            parse_wm_size("Physical size: 1920x1080\nOverride size: 1280x720\n"),
            Some((1280, 720))
        );
        assert_eq!(parse_wm_size("garbage"), None);
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

        let (prog, argv) = cmd_of(Action::BootCompleted);
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        assert_eq!(prog, "waydroid");
        assert_eq!(argv, ["shell", "getprop", "sys.boot_completed"]);

        let (prog, argv) = cmd_of(Action::Maximize {
            package: "com.x.y".into(),
        });
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        assert_eq!(prog, "waydroid");
        assert_eq!(argv, ["shell", "dumpsys", "activity", "activities"]);

        let (prog, argv) = cmd_of(Action::Back);
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        assert_eq!(prog, "waydroid");
        assert_eq!(argv, ["shell", "input", "keyevent", "4"]);

        let (prog, argv) = cmd_of(Action::MaxVolume);
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        assert_eq!(prog, "waydroid");
        assert_eq!(
            argv,
            [
                "shell",
                "--",
                "cmd",
                "media_session",
                "volume",
                "--stream",
                "3",
                "--get"
            ]
        );

        let (prog, argv) = cmd_of(Action::ScaleDensity { permille: 1500 });
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        assert_eq!(prog, "waydroid");
        assert_eq!(argv, ["shell", "wm", "density"]);

        let (prog, argv) = cmd_of(Action::IsRunning {
            package: "com.x.y".into(),
        });
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        assert_eq!(prog, "waydroid");
        assert_eq!(argv, ["shell", "pidof", "com.x.y"]);
    }
}

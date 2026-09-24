//! `lunchbox-wifi-forget <uuid>`: forget a saved Wi-Fi network without letting
//! netplan rewrite `/etc/netplan` (issue #194).
//!
//! Started only as `lunchbox-wifi-forget@<uuid>.service`, as root, by the state
//! custodian, which is granted exactly that by
//! `dist/polkit/50-lunchbox-network.rules`. The one argument is the unit's
//! instance name, so it is validated as strictly as anything read from a
//! client.
//!
//! It does what NetworkManager's delete does, minus the rewrite:
//!
//! 1. remove the profile's definition from the netplan YAML (see the library);
//! 2. `/usr/libexec/netplan/configure --networkmanager-only`, the command
//!    NetworkManager itself runs after a delete, which removes the profile's
//!    generated file under `/run/NetworkManager/system-connections`;
//! 3. `nmcli connection load` on that file, now gone, which makes
//!    NetworkManager drop the profile — and disconnect, if it was in use.
//!    `LoadConnections` is root-only in NetworkManager's D-Bus policy, which is
//!    why this step is here and not in the custodian.
//!
//! If step 2 fails, every file is put back before exiting.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use lunchbox_wifi_forget::files::Plan;
use lunchbox_wifi_forget::{netdef_id, parse_uuid};

const CONFIGURE: &str = "/usr/libexec/netplan/configure";
const NMCLI: &str = "/usr/bin/nmcli";
const GENERATED: &str = "/run/NetworkManager/system-connections";

/// Serialises forgets. systemd runs one start job per unit, but two profiles
/// are two units, and both may need to edit the same hand-written file.
const LOCK: &str = "/run/lock/lunchbox-wifi-forget.lock";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [arg] = args.as_slice() else {
        eprintln!("usage: lunchbox-wifi-forget <uuid>");
        return ExitCode::from(2);
    };
    let Some(uuid) = parse_uuid(arg) else {
        eprintln!("not a NetworkManager profile UUID: {arg:?}");
        return ExitCode::from(2);
    };
    match forget(uuid) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("could not forget {}: {e}", netdef_id(uuid));
            ExitCode::FAILURE
        }
    }
}

fn forget(uuid: &str) -> Result<(), String> {
    let lock = fs::File::create(LOCK).map_err(|e| format!("opening {LOCK}: {e}"))?;
    lock.lock().map_err(|e| format!("locking {LOCK}: {e}"))?;

    let plan = Plan::new(Path::new("/"), uuid)?;
    if plan.is_empty() {
        println!("netplan does not define {}", netdef_id(uuid));
    }
    for change in plan.describe() {
        println!("{change}");
    }
    // Read before regenerating removes them.
    let generated = generated_files(uuid);

    plan.apply().map_err(|e| e.to_string())?;
    if let Err(e) = run(CONFIGURE, &["--networkmanager-only"]) {
        let restored = plan.restore().map_err(|e| e.to_string());
        let _ = run(CONFIGURE, &["--networkmanager-only"]);
        return Err(match restored {
            Ok(()) => format!("{e}; every file was put back"),
            Err(r) => format!("{e}; putting the files back also failed: {r}"),
        });
    }

    if generated.is_empty() {
        // Nothing to name, so ask for everything. Rare: NetworkManager reads
        // netplan profiles from these files, so a saved one has one.
        run(NMCLI, &["connection", "reload"])
    } else {
        let mut args = vec!["connection", "load"];
        args.extend(generated.iter().filter_map(|p| p.to_str()));
        run(NMCLI, &args)
    }
}

/// The files netplan generated for this profile, which NetworkManager loaded it
/// from.
fn generated_files(uuid: &str) -> Vec<PathBuf> {
    let prefix = format!("netplan-{}", netdef_id(uuid));
    let Ok(entries) = fs::read_dir(GENERATED) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    // `-<ssid>.nmconnection` or `.nmconnection` follows the id,
                    // so a UUID that happens to prefix another cannot match.
                    name.strip_prefix(&prefix)
                        .is_some_and(|rest| rest.starts_with('-') || rest.starts_with('.'))
                        && name.ends_with(".nmconnection")
                })
        })
        .collect()
}

/// Run `program`, which is always one of the absolute paths above.
fn run(program: &str, args: &[&str]) -> Result<(), String> {
    // Every caller names an absolute path, so `$PATH` has nothing to choose —
    // and this runs from a system unit, whose environment the custodian cannot
    // set anyway (issue #144).
    #[allow(clippy::disallowed_methods)]
    let status = Command::new(program)
        .args(args)
        .status()
        .map_err(|e| format!("running {program}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} {} failed: {status}", args.join(" ")))
    }
}

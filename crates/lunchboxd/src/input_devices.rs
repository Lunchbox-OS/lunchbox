//! Input-device dependency monitoring for lunchboxd (issue #96).
//!
//! Some activities depend on a particular class of physical input device being
//! attached — the canonical example is a "learn to type" activity that should
//! only appear once a real keyboard is connected to a gaming handheld. The core
//! engine gates such entries on a set of currently-connected
//! [`InputDeviceType`]s; this module is what keeps that set up to date.
//!
//! It mirrors [`InternetMonitor`](crate::internet::InternetMonitor): a
//! background task that runs an initial scan, then re-scans on hotplug and on a
//! slow periodic fallback, feeding results into the engine via
//! [`CoreEngine::set_connected_inputs`] and broadcasting a fresh `StateChanged`
//! whenever the connected set changes.
//!
//! Device classification reuses the same `evdev` heuristics the input-compat
//! bridges use (see `lunchbox-touch-bridge` / `lunchbox-tablet-bridge`).
//! Reading device capabilities requires the lunchboxd user to have read access
//! to `/dev/input/event*` (the `input` group via udev), which the bridges
//! already rely on.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use evdev::{AbsoluteAxisCode, BusType, Device, KeyCode, PropType, RelativeAxisCode};
use lunchbox_api::{Event, EventPayload, InputDeviceType};
use lunchbox_bridge::VIRTUAL_DEVICE_NAME_PREFIX;
use lunchbox_config::Policy;
use lunchbox_core::CoreEngine;
use lunchbox_ipc::IpcServer;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio::time;
use tracing::{debug, info, warn};

/// Directory the kernel exposes input event devices under.
const INPUT_DIR: &str = "/dev/input";

/// Where sysfs lists each input node, linking back to the hardware behind it.
const SYSFS_INPUT_CLASS: &str = "/sys/class/input";

/// Slow fallback re-scan cadence, in case an inotify event is ever missed
/// (e.g. the watcher failed to install). Hotplug is normally reflected far
/// faster via the `/dev/input` watch.
const FALLBACK_RESCAN: Duration = Duration::from_secs(30);

/// Debounce window: a single plug/unplug makes the kernel churn several
/// `/dev/input` nodes (`eventN`, `mouseN`, `jsN`), so we coalesce a burst into
/// one re-scan.
const DEBOUNCE: Duration = Duration::from_millis(250);

/// Background monitor that tracks which input device *types* are connected.
/// Whether anything under `/dev/input` can be read at all.
///
/// The same scan the monitor runs, reduced to the one bit the diagnostic
/// registry needs (issue #143). Blocking; call from `spawn_blocking`.
pub fn inputs_readable() -> bool {
    scan_connected_inputs().is_some()
}

/// Which input device *types* are connected right now, or `None` when nothing
/// under `/dev/input` could be read.
///
/// The same scan again, for diagnostics that depend on what a child can
/// physically do — turning a page needs a key, and a touchscreen has none
/// (issue #160). Blocking; call from `spawn_blocking`.
pub fn connected_inputs() -> Option<HashSet<InputDeviceType>> {
    scan_connected_inputs()
}

/// Runs for the life of the daemon whatever the policy says, because a config
/// reload can add the first `requires_input` entry (issue #236). A monitor
/// built only for a policy that had one at boot never ran for an entry added
/// later, leaving the engine with no detection report and the gate failing
/// open. It re-scans as soon as a reload lands, and does not open
/// `/dev/input` while no entry sets `requires_input`.
pub struct InputMonitor {
    /// Whether we've already logged that detection is unavailable, so the
    /// periodic fallback re-scan doesn't spam the log every cycle.
    warned_unavailable: bool,
}

impl InputMonitor {
    pub fn new() -> Self {
        Self {
            warned_unavailable: false,
        }
    }

    pub async fn run(
        mut self,
        engine: Arc<Mutex<CoreEngine>>,
        ipc: Arc<IpcServer>,
        event_tx: broadcast::Sender<Event>,
    ) {
        // Subscribed before the first scan, so a reload that lands while it
        // runs is still seen.
        let mut events = event_tx.subscribe();

        // Initial scan so the very first gating decision reflects real hardware.
        self.rescan_and_apply(&engine, &ipc, &event_tx).await;

        // Watch /dev/input for hotplug. The `notify` callback is synchronous, so
        // it just nudges an async channel that the loop below drains.
        let (hotplug_tx, mut hotplug_rx) = mpsc::unbounded_channel::<()>();
        let _watcher = match install_watcher(hotplug_tx) {
            Ok(w) => Some(w),
            Err(e) => {
                warn!(
                    error = %e,
                    "Failed to watch {INPUT_DIR} for hotplug; \
                     input-device gating will only refresh on the periodic fallback"
                );
                None
            }
        };

        let mut fallback = time::interval(FALLBACK_RESCAN);
        // Consume the immediate first tick so we don't re-scan right after the
        // initial scan above.
        fallback.tick().await;

        loop {
            tokio::select! {
                Some(()) = hotplug_rx.recv() => {
                    // Coalesce the burst of node changes from a single hotplug,
                    // and let the new device settle before querying it.
                    while hotplug_rx.try_recv().is_ok() {}
                    time::sleep(DEBOUNCE).await;
                    while hotplug_rx.try_recv().is_ok() {}
                    debug!("Re-scanning input devices after /dev/input change");
                    self.rescan_and_apply(&engine, &ipc, &event_tx).await;
                }
                _ = fallback.tick() => {
                    self.rescan_and_apply(&engine, &ipc, &event_tx).await;
                }
                event = events.recv() => match event {
                    Ok(Event { payload: EventPayload::PolicyReloaded { .. }, .. }) => {
                        debug!("Re-scanning input devices after a config reload");
                        self.rescan_and_apply(&engine, &ipc, &event_tx).await;
                    }
                    Ok(_) => {}
                    // A missed event may have been a reload; a scan is cheap.
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        self.rescan_and_apply(&engine, &ipc, &event_tx).await;
                    }
                    // The daemon is shutting down.
                    Err(broadcast::error::RecvError::Closed) => break,
                },
            }
        }
    }

    /// Scan `/dev/input`, push the connected set into the engine, and broadcast
    /// a fresh state snapshot if the set changed. Does nothing while no entry
    /// sets `requires_input`: there is nothing to gate, so `/dev/input` is not
    /// opened.
    async fn rescan_and_apply(
        &mut self,
        engine: &Arc<Mutex<CoreEngine>>,
        ipc: &Arc<IpcServer>,
        event_tx: &broadcast::Sender<Event>,
    ) {
        if !any_entry_requires_input(engine.lock().await.policy()) {
            return;
        }

        // evdev enumeration opens and ioctls each device node; keep that off the
        // async runtime's worker threads.
        let scan = match tokio::task::spawn_blocking(scan_connected_inputs).await {
            Ok(result) => result,
            Err(e) => {
                warn!(error = %e, "Input-device scan task failed");
                return;
            }
        };

        // `None` means not a single input device was readable — almost always
        // because lunchboxd's user lacks `/dev/input` access (the `input`
        // group), not because the machine genuinely has no input. Leaving the
        // engine's set untouched keeps the gate failing open (input-gated
        // entries stay visible) rather than hiding every such activity behind a
        // misconfiguration.
        let Some(connected) = scan else {
            if !self.warned_unavailable {
                self.warned_unavailable = true;
                warn!(
                    "No input devices are readable under {INPUT_DIR}; input-device \
                     dependencies (issue #96) can't be enforced. Add lunchboxd's user \
                     to the `input` group (see docs/INSTALL.md). Gated activities will \
                     stay visible until detection works."
                );
            }
            return;
        };
        self.warned_unavailable = false;

        let changed = {
            let mut eng = engine.lock().await;
            eng.set_connected_inputs(connected.clone())
        };

        if changed {
            let mut types: Vec<&str> = connected.iter().map(|d| d.as_str()).collect();
            types.sort_unstable();
            info!(connected = ?types, "Connected input device types changed");

            let state = { engine.lock().await.get_state() };
            let event = Event::new(EventPayload::StateChanged(state));
            ipc.broadcast_event(event.clone());
            let _ = event_tx.send(event);
        }
    }
}

fn any_entry_requires_input(policy: &Policy) -> bool {
    policy.entries.iter().any(|e| !e.requires_input.is_empty())
}

/// Install an inotify-backed watcher on `/dev/input`. The returned watcher must
/// be kept alive for the callback to keep firing.
fn install_watcher(tx: mpsc::UnboundedSender<()>) -> notify::Result<RecommendedWatcher> {
    let mut watcher = RecommendedWatcher::new(
        move |result: notify::Result<notify::Event>| {
            if let Ok(event) = result {
                // Only device add/remove matters; ignore metadata/access churn.
                if matches!(
                    event.kind,
                    notify::EventKind::Create(_) | notify::EventKind::Remove(_)
                ) {
                    let _ = tx.send(());
                }
            }
        },
        notify::Config::default(),
    )?;
    watcher.watch(Path::new(INPUT_DIR), RecursiveMode::NonRecursive)?;
    info!("Watching {INPUT_DIR} for input-device hotplug");
    Ok(watcher)
}

/// Capability summary extracted from one evdev device, in terms that map
/// directly onto [`InputDeviceType`]. Split out from the raw device so the
/// classification logic is unit-testable without real hardware.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct DeviceCaps {
    /// Relative X and Y axes present (a pointing device that reports motion).
    has_rel_xy: bool,
    /// A pointer button (`BTN_LEFT`) present.
    has_btn_left: bool,
    /// `BTN_TOUCH` present.
    has_btn_touch: bool,
    /// Absolute X and Y axes present.
    has_abs_xy: bool,
    /// `INPUT_PROP_DIRECT` — the device maps onto the screen (a finger
    /// touchscreen) rather than being an indirect tablet/digitizer.
    is_direct: bool,
    /// A representative span of alphabetic typing keys present (distinguishes a
    /// real keyboard from a power button or a few consumer-control keys).
    has_typing_keys: bool,
    /// A gamepad/joystick button present.
    has_gamepad_btn: bool,
}

impl DeviceCaps {
    fn from_device(dev: &Device) -> Self {
        let keys = dev.supported_keys();
        let has_key = |k: KeyCode| keys.map(|s| s.contains(k)).unwrap_or(false);

        let abs = dev.supported_absolute_axes();
        let has_abs = |a: AbsoluteAxisCode| abs.map(|s| s.contains(a)).unwrap_or(false);

        let rel = dev.supported_relative_axes();
        let has_rel = |r: RelativeAxisCode| rel.map(|s| s.contains(r)).unwrap_or(false);

        // A QWERTY top row is a strong, cheap signal of an actual keyboard.
        let has_typing_keys = has_key(KeyCode::KEY_Q)
            && has_key(KeyCode::KEY_W)
            && has_key(KeyCode::KEY_E)
            && has_key(KeyCode::KEY_R)
            && has_key(KeyCode::KEY_T)
            && has_key(KeyCode::KEY_Y);

        Self {
            has_rel_xy: has_rel(RelativeAxisCode::REL_X) && has_rel(RelativeAxisCode::REL_Y),
            has_btn_left: has_key(KeyCode::BTN_LEFT),
            has_btn_touch: has_key(KeyCode::BTN_TOUCH),
            has_abs_xy: has_abs(AbsoluteAxisCode::ABS_X) && has_abs(AbsoluteAxisCode::ABS_Y),
            is_direct: dev.properties().contains(PropType::DIRECT),
            has_typing_keys,
            // `BTN_SOUTH` (== `BTN_GAMEPAD`) marks a game controller;
            // `BTN_TRIGGER` (== `BTN_JOYSTICK`) marks a classic joystick.
            has_gamepad_btn: has_key(KeyCode::BTN_SOUTH) || has_key(KeyCode::BTN_TRIGGER),
        }
    }

    /// Which input device *types* this single device satisfies. A combo device
    /// (e.g. a keyboard with an integrated trackpoint) can satisfy more than
    /// one, so this appends into the caller's accumulating set.
    fn classify_into(self, out: &mut HashSet<InputDeviceType>) {
        // Finger touchscreen: touch button + absolute axes + direct mapping.
        // The `is_direct` check keeps graphics tablets and the absolute
        // "tablet" pointer VMs expose from counting as touch.
        if self.has_btn_touch && self.has_abs_xy && self.is_direct {
            out.insert(InputDeviceType::Touch);
        }
        // Mouse: a relative pointer with a left button.
        if self.has_rel_xy && self.has_btn_left {
            out.insert(InputDeviceType::Mouse);
        }
        // Keyboard: has a real alphabetic key span.
        if self.has_typing_keys {
            out.insert(InputDeviceType::Keyboard);
        }
        // Gamepad / joystick: has a gamepad or joystick button. Checking the
        // button (not just a `js` handler) excludes absolute pointers that the
        // kernel also exposes as a joystick node.
        if self.has_gamepad_btn {
            out.insert(InputDeviceType::Gamepad);
        }
    }
}

/// One device the scan could open: what it is called and what it can do.
///
/// The scan records every device before deciding what any of them count as,
/// because whether a device counts can depend on the others around it.
#[derive(Debug, Clone, Default)]
struct ScannedDevice {
    name: String,
    caps: DeviceCaps,
    /// The sysfs directory of the USB device this node belongs to, when it is
    /// on USB (see [`usb_device_of`]).
    usb_device: Option<PathBuf>,
    /// Whether the firmware marks that USB device's port as one a user plugs
    /// things into. Firmware that does not know says so, which reads as
    /// `false` here.
    usb_removable: bool,
    /// Whether the node is on the i8042 controller — the PS/2 keyboard and
    /// pointer wired inside a laptop or handheld, never one attached later.
    on_i8042: bool,
}

/// The sysfs directory of the USB device an input node belongs to, or `None`
/// when it is not on USB or sysfs does not say.
///
/// A USB device exposes each of its interfaces as its own input node, so a
/// game controller that also emulates a keyboard and a mouse shows up as
/// several nodes; this is what ties them back together. Only USB is followed:
/// a Bluetooth device's ancestors lead to the adapter, which every Bluetooth
/// device shares, so following them would tie unrelated devices together.
fn usb_device_of(node: &Path, bus: BusType) -> Option<PathBuf> {
    if bus != BusType::BUS_USB {
        return None;
    }
    let sysfs = std::fs::canonicalize(Path::new(SYSFS_INPUT_CLASS).join(node.file_name()?)).ok()?;
    // The interface directories between the node and the device (`5-1:1.2`)
    // carry no `idVendor`; the device itself (`5-1`) is the first that does.
    sysfs
        .ancestors()
        .find(|dir| dir.join("idVendor").is_file())
        .map(Path::to_path_buf)
}

/// Whether sysfs marks a USB device as sitting on a user-facing port.
fn usb_removable(usb_device: &Path) -> bool {
    std::fs::read_to_string(usb_device.join("removable"))
        .is_ok_and(|removable| removable.trim() == "removable")
}

/// Which input device types a set of scanned devices adds up to.
///
/// A keyboard or mouse that shares its USB device with a gamepad is the
/// controller emulating one, not one somebody plugged in (issue #236). Gaming
/// handhelds do this with their built-in controller — the Legion Go S exposes
/// two full keyboards and a mouse alongside its gamepad, all on one USB
/// device, as do the Steam Deck and ROG Ally — so counting them would satisfy
/// `requires_input = "keyboard"` on a handheld with nothing attached.
///
/// The same handhelds also have an i8042 AT keyboard, which claims a full
/// keymap whether or not any keys are wired to it. On a laptop that is the
/// real keyboard, so it is only discounted on a handheld, recognised by a
/// controller that emulates a keyboard or mouse on a USB device the firmware
/// does not call removable — a built-in one. The one machine this misreads is
/// a laptop with a Steam Controller's receiver in a port its firmware does
/// not describe, which loses its built-in keyboard while the receiver is in.
///
/// Devices Lunchbox created itself through `/dev/uinput` count as nothing: the
/// HUD's page-turn keyboard stays for the life of the HUD, and an input-compat
/// bridge's keyboard and mouse for the life of its activity, and neither is
/// hardware anybody attached.
fn classify(devices: &[ScannedDevice]) -> HashSet<InputDeviceType> {
    let devices: Vec<&ScannedDevice> = devices
        .iter()
        .filter(|device| !device.name.starts_with(VIRTUAL_DEVICE_NAME_PREFIX))
        .collect();

    let controllers: HashSet<&Path> = devices
        .iter()
        .filter(|device| device.caps.has_gamepad_btn)
        .filter_map(|device| device.usb_device.as_deref())
        .collect();

    let classified: Vec<(&ScannedDevice, HashSet<InputDeviceType>, bool)> = devices
        .iter()
        .map(|&device| {
            let mut types = HashSet::new();
            device.caps.classify_into(&mut types);
            let emulated = device
                .usb_device
                .as_deref()
                .is_some_and(|usb| controllers.contains(usb));
            (device, types, emulated)
        })
        .collect();

    let handheld = classified.iter().any(|(device, types, emulated)| {
        *emulated
            && !device.usb_removable
            && (types.contains(&InputDeviceType::Keyboard)
                || types.contains(&InputDeviceType::Mouse))
    });

    let mut connected = HashSet::new();
    for (device, mut types, emulated) in classified {
        if emulated || (handheld && device.on_i8042) {
            types.remove(&InputDeviceType::Keyboard);
            types.remove(&InputDeviceType::Mouse);
        }
        if !types.is_empty() {
            debug!(name = %device.name, ?types, "Classified input device");
        }
        connected.extend(types);
    }
    connected
}

/// Enumerate `/dev/input` and return the set of connected input device types.
///
/// Returns `None` when not a single device could be enumerated — `evdev`
/// silently skips nodes it can't open, so an empty enumeration almost always
/// means the process lacks `/dev/input` access rather than that the machine has
/// no input hardware. Callers treat `None` as "detection unavailable" and fail
/// open. `Some(set)` (even an empty set) means devices were readable and the
/// set is authoritative.
fn scan_connected_inputs() -> Option<HashSet<InputDeviceType>> {
    let devices: Vec<ScannedDevice> = evdev::enumerate()
        .map(|(path, dev)| {
            let bus = dev.input_id().bus_type();
            let usb_device = usb_device_of(&path, bus);
            ScannedDevice {
                name: dev.name().unwrap_or("").to_string(),
                caps: DeviceCaps::from_device(&dev),
                usb_removable: usb_device.as_deref().is_some_and(usb_removable),
                usb_device,
                on_i8042: bus == BusType::BUS_I8042,
            }
        })
        .collect();
    (!devices.is_empty()).then(|| classify(&devices))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_mouse() {
        let caps = DeviceCaps {
            has_rel_xy: true,
            has_btn_left: true,
            ..Default::default()
        };
        let mut set = HashSet::new();
        caps.classify_into(&mut set);
        assert_eq!(set, HashSet::from([InputDeviceType::Mouse]));
    }

    #[test]
    fn classify_touchscreen_requires_direct() {
        // Absolute touch device that maps onto the screen -> touch.
        let touch = DeviceCaps {
            has_btn_touch: true,
            has_abs_xy: true,
            is_direct: true,
            ..Default::default()
        };
        let mut set = HashSet::new();
        touch.classify_into(&mut set);
        assert_eq!(set, HashSet::from([InputDeviceType::Touch]));

        // Same but indirect (a graphics tablet / VM absolute pointer) -> not
        // counted as a finger touchscreen.
        let tablet = DeviceCaps {
            has_btn_touch: true,
            has_abs_xy: true,
            is_direct: false,
            ..Default::default()
        };
        let mut set = HashSet::new();
        tablet.classify_into(&mut set);
        assert!(set.is_empty());
    }

    #[test]
    fn classify_keyboard() {
        let caps = DeviceCaps {
            has_typing_keys: true,
            ..Default::default()
        };
        let mut set = HashSet::new();
        caps.classify_into(&mut set);
        assert_eq!(set, HashSet::from([InputDeviceType::Keyboard]));
    }

    #[test]
    fn classify_gamepad() {
        let caps = DeviceCaps {
            has_gamepad_btn: true,
            ..Default::default()
        };
        let mut set = HashSet::new();
        caps.classify_into(&mut set);
        assert_eq!(set, HashSet::from([InputDeviceType::Gamepad]));
    }

    #[test]
    fn classify_combo_device() {
        // A keyboard with an integrated pointing stick reports both.
        let caps = DeviceCaps {
            has_rel_xy: true,
            has_btn_left: true,
            has_typing_keys: true,
            ..Default::default()
        };
        let mut set = HashSet::new();
        caps.classify_into(&mut set);
        assert_eq!(
            set,
            HashSet::from([InputDeviceType::Mouse, InputDeviceType::Keyboard])
        );
    }

    #[test]
    fn classify_nothing_for_bare_button() {
        // A power button (a single non-typing key, no axes) satisfies nothing.
        let caps = DeviceCaps::default();
        let mut set = HashSet::new();
        caps.classify_into(&mut set);
        assert!(set.is_empty());
    }

    /// The keyboard, mouse and gamepad nodes a Legion Go S exposes with nothing
    /// attached (issue #236), all on the one USB device its controller is.
    fn legion_go_s_controller() -> Vec<ScannedDevice> {
        let usb = Some(PathBuf::from("/sys/devices/pci0000:00/usb5/5-1"));
        vec![
            ScannedDevice {
                name: "wch.cn Legion Go S".into(),
                caps: DeviceCaps {
                    has_typing_keys: true,
                    ..Default::default()
                },
                usb_device: usb.clone(),
                ..Default::default()
            },
            ScannedDevice {
                name: "Legion Go S".into(),
                caps: DeviceCaps {
                    has_gamepad_btn: true,
                    ..Default::default()
                },
                usb_device: usb.clone(),
                ..Default::default()
            },
            ScannedDevice {
                name: "wch.cn Legion Go S Mouse".into(),
                caps: DeviceCaps {
                    has_rel_xy: true,
                    has_btn_left: true,
                    ..Default::default()
                },
                usb_device: usb.clone(),
                ..Default::default()
            },
            ScannedDevice {
                name: "wch.cn Legion Go S Keyboard".into(),
                caps: DeviceCaps {
                    has_typing_keys: true,
                    ..Default::default()
                },
                usb_device: usb,
                ..Default::default()
            },
        ]
    }

    /// The i8042 keyboard a laptop types on, and a handheld has with no keys.
    fn at_keyboard() -> ScannedDevice {
        ScannedDevice {
            name: "AT Translated Set 2 keyboard".into(),
            caps: DeviceCaps {
                has_typing_keys: true,
                ..Default::default()
            },
            on_i8042: true,
            ..Default::default()
        }
    }

    fn usb_keyboard(usb_device: &str) -> ScannedDevice {
        ScannedDevice {
            name: "USB Keyboard".into(),
            caps: DeviceCaps {
                has_typing_keys: true,
                ..Default::default()
            },
            usb_device: Some(PathBuf::from(usb_device)),
            ..Default::default()
        }
    }

    #[test]
    fn lunchbox_virtual_devices_count_as_nothing() {
        let devices = [
            ScannedDevice {
                name: format!("{VIRTUAL_DEVICE_NAME_PREFIX}keyboard"),
                caps: DeviceCaps {
                    has_typing_keys: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            ScannedDevice {
                name: format!("{VIRTUAL_DEVICE_NAME_PREFIX}pointer+keyboard"),
                caps: DeviceCaps {
                    has_typing_keys: true,
                    has_rel_xy: true,
                    has_btn_left: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        ];
        assert!(classify(&devices).is_empty());
    }

    #[test]
    fn handheld_at_keyboard_is_not_a_keyboard() {
        let mut devices = legion_go_s_controller();
        devices.push(at_keyboard());
        assert_eq!(
            classify(&devices),
            HashSet::from([InputDeviceType::Gamepad])
        );
    }

    #[test]
    fn laptop_at_keyboard_counts_beside_a_plain_gamepad() {
        // An external pad that emulates nothing says nothing about the
        // machine it is plugged into.
        let devices = [
            at_keyboard(),
            ScannedDevice {
                caps: DeviceCaps {
                    has_gamepad_btn: true,
                    ..Default::default()
                },
                usb_device: Some(PathBuf::from("/sys/devices/pci0000:00/usb1/1-3")),
                ..Default::default()
            },
        ];
        assert_eq!(
            classify(&devices),
            HashSet::from([InputDeviceType::Gamepad, InputDeviceType::Keyboard])
        );
    }

    #[test]
    fn laptop_at_keyboard_counts_beside_a_removable_emulating_controller() {
        // A Steam Controller in a port the firmware calls removable emulates a
        // keyboard too, but is not built in.
        let mut devices = legion_go_s_controller();
        for device in &mut devices {
            device.usb_removable = true;
        }
        devices.push(at_keyboard());
        assert_eq!(
            classify(&devices),
            HashSet::from([InputDeviceType::Gamepad, InputDeviceType::Keyboard])
        );
    }

    #[test]
    fn controller_emulation_is_not_a_keyboard_or_mouse() {
        assert_eq!(
            classify(&legion_go_s_controller()),
            HashSet::from([InputDeviceType::Gamepad])
        );
    }

    #[test]
    fn keyboard_on_another_usb_device_still_counts() {
        let mut devices = legion_go_s_controller();
        devices.push(usb_keyboard("/sys/devices/pci0000:00/usb3/3-2"));
        assert_eq!(
            classify(&devices),
            HashSet::from([InputDeviceType::Gamepad, InputDeviceType::Keyboard])
        );
    }

    #[test]
    fn keyboard_without_a_usb_device_is_never_tied_to_a_controller() {
        // A Bluetooth keyboard next to a Bluetooth gamepad: neither is on USB,
        // so neither is taken for the other's emulation.
        let devices = [
            ScannedDevice {
                caps: DeviceCaps {
                    has_typing_keys: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            ScannedDevice {
                caps: DeviceCaps {
                    has_gamepad_btn: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        ];
        assert_eq!(
            classify(&devices),
            HashSet::from([InputDeviceType::Gamepad, InputDeviceType::Keyboard])
        );
    }

    /// Smoke test against the machine's real `/dev/input`. Ignored by default —
    /// it depends on attached hardware and read access to `/dev/input`, so it is
    /// not suitable for CI. Run manually with:
    /// `cargo test -p lunchboxd --bin lunchboxd scan_real_devices -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn scan_real_devices() {
        match scan_connected_inputs() {
            Some(connected) => println!("detected input device types: {connected:?}"),
            None => println!("no input devices readable (need /dev/input access / `input` group)"),
        }
    }
}

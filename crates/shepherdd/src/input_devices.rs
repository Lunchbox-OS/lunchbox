//! Input-device dependency monitoring for shepherdd (issue #96).
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
//! bridges use (see `shepherd-touch-bridge` / `shepherd-tablet-bridge`).
//! Reading device capabilities requires the shepherdd user to have read access
//! to `/dev/input/event*` (the `input` group via udev), which the bridges
//! already rely on.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use evdev::{AbsoluteAxisCode, Device, KeyCode, PropType, RelativeAxisCode};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use shepherd_api::{Event, EventPayload, InputDeviceType};
use shepherd_config::Policy;
use shepherd_core::CoreEngine;
use shepherd_ipc::IpcServer;
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio::time;
use tracing::{debug, info, warn};

/// Directory the kernel exposes input event devices under.
const INPUT_DIR: &str = "/dev/input";

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

pub struct InputMonitor {
    /// Whether we've already logged that detection is unavailable, so the
    /// periodic fallback re-scan doesn't spam the log every cycle.
    warned_unavailable: bool,
}

impl InputMonitor {
    /// Build a monitor only if some entry actually depends on an input device.
    /// When no entry sets `requires_input`, there is nothing to gate, so we
    /// avoid opening `/dev/input` entirely.
    pub fn from_policy(policy: &Policy) -> Option<Self> {
        let any_required = policy.entries.iter().any(|e| !e.requires_input.is_empty());
        any_required.then_some(Self {
            warned_unavailable: false,
        })
    }

    pub async fn run(
        mut self,
        engine: Arc<Mutex<CoreEngine>>,
        ipc: Arc<IpcServer>,
        event_tx: broadcast::Sender<Event>,
    ) {
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
                else => break,
            }
        }
    }

    /// Scan `/dev/input`, push the connected set into the engine, and broadcast
    /// a fresh state snapshot if the set changed.
    async fn rescan_and_apply(
        &mut self,
        engine: &Arc<Mutex<CoreEngine>>,
        ipc: &Arc<IpcServer>,
        event_tx: &broadcast::Sender<Event>,
    ) {
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
        // because shepherdd's user lacks `/dev/input` access (the `input`
        // group), not because the machine genuinely has no input. Leaving the
        // engine's set untouched keeps the gate failing open (input-gated
        // entries stay visible) rather than hiding every such activity behind a
        // misconfiguration.
        let Some(connected) = scan else {
            if !self.warned_unavailable {
                self.warned_unavailable = true;
                warn!(
                    "No input devices are readable under {INPUT_DIR}; input-device \
                     dependencies (issue #96) can't be enforced. Add shepherdd's user \
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

/// Enumerate `/dev/input` and return the set of connected input device types.
///
/// Returns `None` when not a single device could be enumerated — `evdev`
/// silently skips nodes it can't open, so an empty enumeration almost always
/// means the process lacks `/dev/input` access rather than that the machine has
/// no input hardware. Callers treat `None` as "detection unavailable" and fail
/// open. `Some(set)` (even an empty set) means devices were readable and the
/// set is authoritative.
fn scan_connected_inputs() -> Option<HashSet<InputDeviceType>> {
    let mut connected = HashSet::new();
    let mut seen_any = false;
    for (path, dev) in evdev::enumerate() {
        seen_any = true;
        let caps = DeviceCaps::from_device(&dev);
        let before = connected.len();
        caps.classify_into(&mut connected);
        if connected.len() != before {
            debug!(
                path = %path.display(),
                name = %dev.name().unwrap_or(""),
                "Classified input device"
            );
        }
        if connected.len() == 4 {
            // All four types seen; no need to keep opening devices.
            break;
        }
    }
    seen_any.then_some(connected)
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

    /// Smoke test against the machine's real `/dev/input`. Ignored by default —
    /// it depends on attached hardware and read access to `/dev/input`, so it is
    /// not suitable for CI. Run manually with:
    /// `cargo test -p shepherdd --bin shepherdd scan_real_devices -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn scan_real_devices() {
        match scan_connected_inputs() {
            Some(connected) => println!("detected input device types: {connected:?}"),
            None => println!("no input devices readable (need /dev/input access / `input` group)"),
        }
    }
}

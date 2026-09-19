//! lunchbox-touch-bridge: translate touchscreen input to synthetic mouse
//! events.
//!
//! Run alongside an activity that ignores raw touch events; the bridge grabs
//! every touchscreen, converts touch events to absolute pointer motion and
//! `BTN_LEFT` press/release events on a `/dev/uinput` virtual pointer, and
//! exits on SIGTERM. The uinput backend works on any Wayland compositor (and
//! X11), unlike the wlroots-only virtual-pointer protocol used before (issue
//! #58). Releasing the grabs is handled by the kernel when the file
//! descriptors close at process exit.
//!
//! With `--grab-only` the bridge instead just grabs every touchscreen and
//! discards its events, with no synthetic output at all — effectively
//! disabling the touchscreen for the duration of an activity (issue #68). No
//! `/dev/uinput` access is needed in this mode.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use evdev::{AbsoluteAxisCode, Device, EventSummary, KeyCode, PropType, SynchronizationCode};
use lunchbox_bridge::{OutputEvent, OutputSink, UinputSink};
use tracing::{debug, info, warn};

const BTN_LEFT: u32 = 0x110;

#[derive(Parser, Debug)]
#[command(
    name = "lunchbox-touch-bridge",
    about = "Grab touchscreens and emit Wayland virtual-pointer events"
)]
struct Args {
    /// Touchscreen device path. Repeat for multiple devices. If omitted,
    /// every touchscreen under /dev/input is auto-detected.
    #[arg(long = "device", value_name = "PATH")]
    devices: Vec<PathBuf>,

    /// Grab every touchscreen and discard its events instead of translating
    /// them to pointer motion. Disables the touchscreen for the lifetime of
    /// the bridge; no synthetic events are emitted and no `/dev/uinput`
    /// access is required.
    #[arg(long = "grab-only", default_value_t = false)]
    grab_only: bool,
}

/// Touch state update emitted by reader threads.
///
/// Coordinates are already normalized against *the emitting device's* own
/// axis range (see [`DeviceRange::normalize`]). Normalizing in the reader
/// rather than in the main loop is what keeps a second grabbed device from
/// imposing its range on everyone else's coordinates.
#[derive(Debug, Clone, Copy)]
enum TouchUpdate {
    /// Finger down at normalized coordinates.
    Down {
        x: u32,
        y: u32,
        x_extent: u32,
        y_extent: u32,
    },
    /// Finger moved while still down.
    Move {
        x: u32,
        y: u32,
        x_extent: u32,
        y_extent: u32,
    },
    /// Finger up.
    Up,
}

/// Per-device coordinate range used for normalizing positions.
#[derive(Debug, Clone, Copy)]
struct DeviceRange {
    x_min: i32,
    x_max: i32,
    y_min: i32,
    y_max: i32,
}

impl DeviceRange {
    fn from_device(dev: &Device) -> Result<Self> {
        let abs = dev
            .get_absinfo()
            .context("failed to read absolute axis info")?;
        let mut x = None;
        let mut y = None;
        for (axis, info) in abs {
            if axis == AbsoluteAxisCode::ABS_X {
                x = Some((info.minimum(), info.maximum()));
            } else if axis == AbsoluteAxisCode::ABS_Y {
                y = Some((info.minimum(), info.maximum()));
            }
        }
        let (x_min, x_max) = x.ok_or_else(|| anyhow!("device has no ABS_X axis"))?;
        let (y_min, y_max) = y.ok_or_else(|| anyhow!("device has no ABS_Y axis"))?;
        Ok(Self {
            x_min,
            x_max,
            y_min,
            y_max,
        })
    }

    /// Convert a raw device coordinate into a (value, extent) pair suitable
    /// for `motion_absolute`. Both fall back to safe defaults if the axis
    /// reports a degenerate range.
    fn normalize(&self, raw_x: i32, raw_y: i32) -> (u32, u32, u32, u32) {
        let (x, x_extent) = normalize_axis(raw_x, self.x_min, self.x_max);
        let (y, y_extent) = normalize_axis(raw_y, self.y_min, self.y_max);
        (x, y, x_extent, y_extent)
    }
}

fn normalize_axis(raw: i32, min: i32, max: i32) -> (u32, u32) {
    let extent = (max - min).max(1) as u32;
    let value = raw.saturating_sub(min).clamp(0, max - min) as u32;
    (value, extent)
}

/// The capability bits that decide whether a device is a touchscreen.
/// Split out from [`looks_like_touchscreen`] so the rule itself is testable
/// without a real `/dev/input` node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TouchCaps {
    /// Reports `BTN_TOUCH`.
    btn_touch: bool,
    /// Reports both `ABS_X` and `ABS_Y`.
    abs_xy: bool,
    /// Declares `INPUT_PROP_DIRECT`: you touch the surface you look at.
    direct: bool,
    /// Declares `INPUT_PROP_POINTER`: an indirect device (touchpad, tablet).
    pointer: bool,
    /// Reports `BTN_TOOL_FINGER`, which touchpads set and panels do not.
    tool_finger: bool,
}

/// True if the capabilities describe a *direct* touchscreen — a panel you
/// touch — rather than an indirect absolute pointer.
///
/// `BTN_TOUCH` plus absolute X/Y is not enough on its own: clickpads report
/// exactly that too. On the Legion Go S the touchpad
/// (`INPUT_PROP_POINTER | INPUT_PROP_BUTTONPAD`, range `0..400`) matched the
/// old test and got grabbed alongside the 1920x1200 panel, so whichever node
/// `readdir` happened to yield first decided the coordinate mapping for both.
/// The extra properties mirror how udev's `input_id` builtin classifies these
/// devices: `INPUT_PROP_DIRECT` marks a touchscreen outright, and a device
/// that claims neither `INPUT_PROP_POINTER` nor `BTN_TOOL_FINGER` is treated
/// as one as well, so panels that omit the property still work.
fn is_direct_touchscreen(caps: TouchCaps) -> bool {
    if !(caps.btn_touch && caps.abs_xy) {
        return false;
    }
    caps.direct || !(caps.pointer || caps.tool_finger)
}

/// Read a device's [`TouchCaps`] and apply [`is_direct_touchscreen`].
fn looks_like_touchscreen(dev: &Device) -> bool {
    let keys = dev.supported_keys();
    let caps = TouchCaps {
        btn_touch: keys
            .map(|k| k.contains(KeyCode::BTN_TOUCH))
            .unwrap_or(false),
        abs_xy: dev
            .supported_absolute_axes()
            .map(|axes| {
                axes.contains(AbsoluteAxisCode::ABS_X) && axes.contains(AbsoluteAxisCode::ABS_Y)
            })
            .unwrap_or(false),
        direct: dev.properties().contains(PropType::DIRECT),
        pointer: dev.properties().contains(PropType::POINTER),
        tool_finger: keys
            .map(|k| k.contains(KeyCode::BTN_TOOL_FINGER))
            .unwrap_or(false),
    };
    is_direct_touchscreen(caps)
}

fn discover_touchscreens() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for (path, dev) in evdev::enumerate() {
        if looks_like_touchscreen(&dev) {
            debug!(path = %path.display(), name = %dev.name().unwrap_or(""), "Found touchscreen");
            paths.push(path);
        } else {
            debug!(path = %path.display(), name = %dev.name().unwrap_or(""), "Not a direct touchscreen; skipping");
        }
    }
    paths
}

/// Open a touchscreen, grab it, and start a reader thread. The thread sends
/// `TouchUpdate`s through `tx` until the device disappears or `shutdown`
/// flips to true.
fn spawn_device_reader(
    path: &Path,
    tx: Sender<TouchUpdate>,
    shutdown: Arc<AtomicBool>,
) -> Result<thread::JoinHandle<()>> {
    let mut dev =
        Device::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let range = DeviceRange::from_device(&dev)?;
    dev.grab()
        .with_context(|| format!("failed to EVIOCGRAB {}", path.display()))?;

    let name = dev.name().unwrap_or("").to_string();
    info!(
        path = %path.display(),
        name = %name,
        x_range = format!("{}..{}", range.x_min, range.x_max),
        y_range = format!("{}..{}", range.y_min, range.y_max),
        "Grabbed touchscreen"
    );

    let path_buf = path.to_path_buf();
    let handle = thread::Builder::new()
        .name(format!("touch-{}", path_buf.display()))
        .spawn(move || device_loop(dev, path_buf, range, tx, shutdown))?;
    Ok(handle)
}

fn device_loop(
    mut dev: Device,
    path: PathBuf,
    range: DeviceRange,
    tx: Sender<TouchUpdate>,
    shutdown: Arc<AtomicBool>,
) {
    // Per-finger state. We only emit events for the first active touch
    // (single-finger model); additional fingers are ignored.
    let mut last_x = (range.x_min + range.x_max) / 2;
    let mut last_y = (range.y_min + range.y_max) / 2;
    let mut pending_x = last_x;
    let mut pending_y = last_y;
    let mut pressed = false;
    let mut pending_press: Option<bool> = None;

    while !shutdown.load(Ordering::Relaxed) {
        let events = match dev.fetch_events() {
            Ok(events) => events,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => {
                warn!(path = %path.display(), error = %e, "evdev read failed; exiting reader");
                return;
            }
        };

        for ev in events {
            match ev.destructure() {
                EventSummary::Key(_, KeyCode::BTN_TOUCH, value) => {
                    pending_press = Some(value != 0);
                }
                EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_X, value) => {
                    pending_x = value;
                }
                EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_Y, value) => {
                    pending_y = value;
                }
                EventSummary::Synchronization(_, SynchronizationCode::SYN_REPORT, _) => {
                    // Frame boundary — emit at most one update per frame.
                    let target_press = pending_press.unwrap_or(pressed);
                    let moved = pending_x != last_x || pending_y != last_y;

                    if target_press && !pressed {
                        let (x, y, x_extent, y_extent) = range.normalize(pending_x, pending_y);
                        let _ = tx.send(TouchUpdate::Down {
                            x,
                            y,
                            x_extent,
                            y_extent,
                        });
                    } else if target_press && pressed && moved {
                        let (x, y, x_extent, y_extent) = range.normalize(pending_x, pending_y);
                        let _ = tx.send(TouchUpdate::Move {
                            x,
                            y,
                            x_extent,
                            y_extent,
                        });
                    } else if !target_press && pressed {
                        let _ = tx.send(TouchUpdate::Up);
                    }

                    pressed = target_press;
                    last_x = pending_x;
                    last_y = pending_y;
                    pending_press = None;
                }
                _ => {}
            }
        }
    }
}

fn install_signal_handlers(shutdown: Arc<AtomicBool>) -> Result<()> {
    use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};

    static SHUTDOWN_FLAG: AtomicBool = AtomicBool::new(false);
    extern "C" fn handler(_: i32) {
        SHUTDOWN_FLAG.store(true, Ordering::SeqCst);
    }

    let action = SigAction::new(
        SigHandler::Handler(handler),
        SaFlags::empty(),
        SigSet::empty(),
    );
    // SAFETY: Installing a signal handler that only sets an atomic flag is
    // async-signal-safe.
    unsafe {
        sigaction(Signal::SIGTERM, &action)?;
        sigaction(Signal::SIGINT, &action)?;
    }

    // Bridge the static flag into the heap-allocated atomic.
    thread::Builder::new()
        .name("touch-bridge-signals".into())
        .spawn(move || {
            loop {
                if SHUTDOWN_FLAG.load(Ordering::SeqCst) {
                    shutdown.store(true, Ordering::SeqCst);
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
        })?;
    Ok(())
}

fn millis_since(start: Instant) -> u32 {
    start.elapsed().as_millis() as u32
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let shutdown = Arc::new(AtomicBool::new(false));
    install_signal_handlers(shutdown.clone())?;

    let device_paths = if args.devices.is_empty() {
        discover_touchscreens()
    } else {
        args.devices.clone()
    };

    if device_paths.is_empty() {
        return Err(anyhow!(
            "no touchscreen devices found; the user running this bridge must \
             be in the 'input' group, or pass --device explicitly"
        ));
    }

    // Each reader normalizes against its own device's axis range before
    // sending, so grabbing more than one device can't make one device's range
    // govern another's coordinates.
    let (tx, rx) = mpsc::channel::<TouchUpdate>();
    let mut readers: Vec<thread::JoinHandle<()>> = Vec::new();
    for path in &device_paths {
        match spawn_device_reader(path, tx.clone(), shutdown.clone()) {
            Ok(handle) => readers.push(handle),
            Err(e) => warn!(path = %path.display(), error = %e, "failed to start reader"),
        }
    }
    drop(tx);

    if readers.is_empty() {
        return Err(anyhow!("failed to grab any touchscreen device"));
    }

    // In grab-only mode the grabs alone disable the touchscreen; we just hold
    // them and drain (discard) the reader updates until shutdown. No uinput
    // device is created, so this mode works without /dev/uinput access.
    if args.grab_only {
        info!("Touchscreen disabled (grab-only); discarding all touch events");
        run_grab_only_loop(rx, &shutdown);
        info!("Releasing touchscreen grab");
        return Ok(());
    }

    let mut sink = UinputSink::new_absolute().context("failed to create uinput pointer")?;

    info!("Touch-to-mouse bridge ready");

    let start = Instant::now();
    run_main_loop(rx, &mut sink, start, &shutdown)?;

    info!("Shutting down touch-to-mouse bridge");
    let _ = sink.flush();
    Ok(())
}

/// Grab-only main loop: wait until shutdown, discarding every touch update the
/// reader threads produce. The kernel `EVIOCGRAB` (held by the readers) keeps
/// the touchscreen events from reaching the activity; we simply throw them
/// away rather than synthesizing anything.
fn run_grab_only_loop(rx: Receiver<TouchUpdate>, shutdown: &AtomicBool) {
    while !shutdown.load(Ordering::SeqCst) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(_) => {} // Discard: the point is to swallow touch input.
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                debug!("All device readers exited; shutting down");
                return;
            }
        }
    }
}

fn run_main_loop(
    rx: Receiver<TouchUpdate>,
    sink: &mut UinputSink,
    start: Instant,
    shutdown: &AtomicBool,
) -> Result<()> {
    while !shutdown.load(Ordering::SeqCst) {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(update) => {
                let time = millis_since(start);
                emit_update(sink, time, update);
                sink.flush()?;
            }
            Err(RecvTimeoutError::Timeout) => {
                sink.flush()?;
            }
            Err(RecvTimeoutError::Disconnected) => {
                debug!("All device readers exited; shutting down");
                return Ok(());
            }
        }
    }
    Ok(())
}

fn emit_update(sink: &mut UinputSink, time: u32, update: TouchUpdate) {
    match update {
        TouchUpdate::Down {
            x,
            y,
            x_extent,
            y_extent,
        } => {
            sink.dispatch(
                OutputEvent::PointerMotionAbsolute {
                    x,
                    y,
                    x_extent,
                    y_extent,
                },
                time,
            );
            sink.dispatch(
                OutputEvent::PointerButton {
                    button: BTN_LEFT,
                    pressed: true,
                },
                time,
            );
            sink.frame();
        }
        TouchUpdate::Move {
            x,
            y,
            x_extent,
            y_extent,
        } => {
            sink.dispatch(
                OutputEvent::PointerMotionAbsolute {
                    x,
                    y,
                    x_extent,
                    y_extent,
                },
                time,
            );
            sink.frame();
        }
        TouchUpdate::Up => {
            sink.dispatch(
                OutputEvent::PointerButton {
                    button: BTN_LEFT,
                    pressed: false,
                },
                time,
            );
            sink.frame();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A capability set that satisfies the button/axis test, so each case
    /// below varies only the property bits that decide direct vs indirect.
    fn caps() -> TouchCaps {
        TouchCaps {
            btn_touch: true,
            abs_xy: true,
            direct: false,
            pointer: false,
            tool_finger: false,
        }
    }

    #[test]
    fn direct_panel_is_a_touchscreen() {
        // NVTK0603 panel on the Legion Go S: INPUT_PROP_DIRECT.
        assert!(is_direct_touchscreen(TouchCaps {
            direct: true,
            ..caps()
        }));
    }

    #[test]
    fn panel_without_the_direct_property_is_still_a_touchscreen() {
        // Claims neither INPUT_PROP_POINTER nor BTN_TOOL_FINGER, so there is
        // nothing marking it indirect.
        assert!(is_direct_touchscreen(caps()));
    }

    #[test]
    fn clickpad_is_not_a_touchscreen() {
        // The regression: the Legion Go S touchpad reports BTN_TOUCH and
        // absolute X/Y, but is INPUT_PROP_POINTER | INPUT_PROP_BUTTONPAD.
        assert!(!is_direct_touchscreen(TouchCaps {
            pointer: true,
            tool_finger: true,
            ..caps()
        }));
    }

    #[test]
    fn touchpad_without_the_pointer_property_is_rejected_on_tool_finger() {
        assert!(!is_direct_touchscreen(TouchCaps {
            tool_finger: true,
            ..caps()
        }));
    }

    #[test]
    fn direct_wins_over_the_indirect_hints() {
        // A panel that sets DIRECT *and* reports BTN_TOOL_FINGER is still a
        // touchscreen; DIRECT is the authoritative bit.
        assert!(is_direct_touchscreen(TouchCaps {
            direct: true,
            tool_finger: true,
            ..caps()
        }));
    }

    #[test]
    fn button_and_axis_bits_are_still_required() {
        assert!(!is_direct_touchscreen(TouchCaps {
            btn_touch: false,
            direct: true,
            ..caps()
        }));
        assert!(!is_direct_touchscreen(TouchCaps {
            abs_xy: false,
            direct: true,
            ..caps()
        }));
    }

    #[test]
    fn each_device_normalizes_against_its_own_range() {
        // Two grabbed devices with very different ranges: a 1920x1200 panel
        // and a 400x400 clickpad. Before the fix a single shared range was
        // applied to both, so the panel's coordinates were divided by 400 and
        // clamped -- the top 400/1920 of the panel covered the whole screen.
        let panel = DeviceRange {
            x_min: 0,
            x_max: 1920,
            y_min: 0,
            y_max: 1200,
        };
        let pad = DeviceRange {
            x_min: 0,
            x_max: 400,
            y_min: 0,
            y_max: 400,
        };

        // Mid-panel stays mid-range rather than saturating.
        let (x, y, xe, ye) = panel.normalize(960, 600);
        assert_eq!((x, xe), (960, 1920));
        assert_eq!((y, ye), (600, 1200));

        // The pad keeps its own, much smaller extent.
        let (x, _, xe, _) = pad.normalize(200, 200);
        assert_eq!((x, xe), (200, 400));

        // The old behavior, for contrast: the panel's coordinate normalized
        // against the pad's range saturates well before the panel's edge.
        let (x, _, xe, _) = pad.normalize(960, 600);
        assert_eq!((x, xe), (400, 400));
    }

    #[test]
    fn normalize_axis_basic() {
        let (v, e) = normalize_axis(50, 0, 100);
        assert_eq!(v, 50);
        assert_eq!(e, 100);
    }

    #[test]
    fn normalize_axis_offset_min() {
        let (v, e) = normalize_axis(150, 100, 200);
        assert_eq!(v, 50);
        assert_eq!(e, 100);
    }

    #[test]
    fn normalize_axis_clamps_below_min() {
        let (v, _) = normalize_axis(-10, 0, 100);
        assert_eq!(v, 0);
    }

    #[test]
    fn normalize_axis_clamps_above_max() {
        let (v, _) = normalize_axis(150, 0, 100);
        assert_eq!(v, 100);
    }

    #[test]
    fn normalize_axis_degenerate_range() {
        // If the device reports min == max, we still produce a valid extent.
        let (v, e) = normalize_axis(5, 5, 5);
        assert_eq!(e, 1);
        assert_eq!(v, 0);
    }
}

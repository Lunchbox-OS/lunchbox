//! shepherd-touch-bridge: translate touchscreen input to synthetic mouse
//! events.
//!
//! Run alongside an activity that ignores raw touch events; the bridge grabs
//! every touchscreen, converts touch events to absolute pointer motion and
//! `BTN_LEFT` press/release events on a `/dev/uinput` virtual pointer, and
//! exits on SIGTERM. The uinput backend works on any Wayland compositor (and
//! X11), unlike the wlroots-only virtual-pointer protocol used before (issue
//! #58). Releasing the grabs is handled by the kernel when the file
//! descriptors close at process exit.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use evdev::{AbsoluteAxisCode, Device, EventSummary, KeyCode, SynchronizationCode};
use shepherd_bridge::{OutputEvent, OutputSink, UinputSink};
use tracing::{debug, info, warn};

const BTN_LEFT: u32 = 0x110;

#[derive(Parser, Debug)]
#[command(
    name = "shepherd-touch-bridge",
    about = "Grab touchscreens and emit Wayland virtual-pointer events"
)]
struct Args {
    /// Touchscreen device path. Repeat for multiple devices. If omitted,
    /// every touchscreen under /dev/input is auto-detected.
    #[arg(long = "device", value_name = "PATH")]
    devices: Vec<PathBuf>,

    /// Compositor output scale (e.g. 1.5). Absolute coordinates are divided
    /// by this so the cursor lands in logical, not physical, pixels. Defaults
    /// to 1.0 (no scaling); shepherd-launcher passes the live sway scale.
    #[arg(long = "output-scale", default_value_t = 1.0)]
    output_scale: f64,
}

/// Touch state update emitted by reader threads.
#[derive(Debug, Clone, Copy)]
enum TouchUpdate {
    /// Finger down at absolute device coordinates.
    Down { x: i32, y: i32 },
    /// Finger moved while still down.
    Move { x: i32, y: i32 },
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

/// True if the device has both `BTN_TOUCH` and absolute X/Y axes — i.e.,
/// looks like a touchscreen.
fn looks_like_touchscreen(dev: &Device) -> bool {
    let has_btn = dev
        .supported_keys()
        .map(|keys| keys.contains(KeyCode::BTN_TOUCH))
        .unwrap_or(false);
    let has_abs = dev
        .supported_absolute_axes()
        .map(|axes| {
            axes.contains(AbsoluteAxisCode::ABS_X) && axes.contains(AbsoluteAxisCode::ABS_Y)
        })
        .unwrap_or(false);
    has_btn && has_abs
}

fn discover_touchscreens() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for (path, dev) in evdev::enumerate() {
        if looks_like_touchscreen(&dev) {
            debug!(path = %path.display(), name = %dev.name().unwrap_or(""), "Found touchscreen");
            paths.push(path);
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
                        let _ = tx.send(TouchUpdate::Down {
                            x: pending_x,
                            y: pending_y,
                        });
                    } else if target_press && pressed && moved {
                        let _ = tx.send(TouchUpdate::Move {
                            x: pending_x,
                            y: pending_y,
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

    // Per-device range used for normalizing pointer events. We pick the
    // range from the first device that successfully opens; the kernel grab
    // ensures the focused device's events are the only ones we handle.
    let mut device_range: Option<DeviceRange> = None;
    let (tx, rx) = mpsc::channel::<TouchUpdate>();
    let mut readers: Vec<thread::JoinHandle<()>> = Vec::new();
    for path in &device_paths {
        if device_range.is_none()
            && let Ok(dev) = Device::open(path)
            && let Ok(range) = DeviceRange::from_device(&dev)
        {
            device_range = Some(range);
        }
        match spawn_device_reader(path, tx.clone(), shutdown.clone()) {
            Ok(handle) => readers.push(handle),
            Err(e) => warn!(path = %path.display(), error = %e, "failed to start reader"),
        }
    }
    drop(tx);

    if readers.is_empty() {
        return Err(anyhow!("failed to grab any touchscreen device"));
    }

    let device_range = device_range.ok_or_else(|| anyhow!("no usable device range"))?;

    let mut sink =
        UinputSink::new_absolute(args.output_scale).context("failed to create uinput pointer")?;

    info!("Touch-to-mouse bridge ready");

    let start = Instant::now();
    run_main_loop(rx, &mut sink, device_range, start, &shutdown)?;

    info!("Shutting down touch-to-mouse bridge");
    let _ = sink.flush();
    Ok(())
}

fn run_main_loop(
    rx: Receiver<TouchUpdate>,
    sink: &mut UinputSink,
    range: DeviceRange,
    start: Instant,
    shutdown: &AtomicBool,
) -> Result<()> {
    while !shutdown.load(Ordering::SeqCst) {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(update) => {
                let time = millis_since(start);
                emit_update(sink, range, time, update);
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

fn emit_update(sink: &mut UinputSink, range: DeviceRange, time: u32, update: TouchUpdate) {
    match update {
        TouchUpdate::Down { x, y } => {
            let (nx, ny, xe, ye) = range.normalize(x, y);
            sink.dispatch(
                OutputEvent::PointerMotionAbsolute {
                    x: nx,
                    y: ny,
                    x_extent: xe,
                    y_extent: ye,
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
        TouchUpdate::Move { x, y } => {
            let (nx, ny, xe, ye) = range.normalize(x, y);
            sink.dispatch(
                OutputEvent::PointerMotionAbsolute {
                    x: nx,
                    y: ny,
                    x_extent: xe,
                    y_extent: ye,
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

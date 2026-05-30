//! `/dev/uinput` output backend.
//!
//! Creates a kernel virtual input device and emits synthesized events into it.
//! Unlike the wlroots virtual-pointer/keyboard protocols the bridges used
//! before, a uinput device is consumed by *every* compositor through libinput
//! — wlroots, Mutter, KWin, and X11 — so the bridges work in any session
//! (issue #58).
//!
//! Two device shapes are offered: a relative pointer + keyboard
//! ([`UinputSink::new_relative`], gamepad bridge) and an absolute pointer
//! ([`UinputSink::new_absolute`], touch bridge). A device may not be both a
//! relative and an absolute pointer without confusing libinput, hence the
//! split.

use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, EventType, InputEvent, KeyCode, RelativeAxisCode,
    UinputAbsSetup,
};

use crate::event::{OutputEvent, ScrollAxis};
use crate::sink::OutputSink;

// evdev event-type selectors (`InputEvent::new` takes the raw `u16`).
const EV_KEY: u16 = EventType::KEY.0;
const EV_REL: u16 = EventType::RELATIVE.0;
const EV_ABS: u16 = EventType::ABSOLUTE.0;
const EV_SYN: u16 = EventType::SYNCHRONIZATION.0;

// Raw evdev codes we emit. See linux/input-event-codes.h.
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_HWHEEL: u16 = 0x06;
const REL_WHEEL: u16 = 0x08;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const SYN_REPORT: u16 = 0x00;

// Mouse button range (BTN_LEFT..BTN_TASK).
const BTN_MOUSE_FIRST: u16 = 0x110;
const BTN_MOUSE_LAST: u16 = 0x117;

/// Logical resolution of the absolute pointer. Touch coordinates are rescaled
/// into `0..=ABS_MAX` so the device can declare a fixed range up front.
const ABS_MAX: i32 = 65535;

/// How long to wait after creating the device for udev/libinput to bind it.
/// Events emitted before the compositor opens the node are silently dropped,
/// so the first real input would otherwise be lost.
const SETTLE: Duration = Duration::from_millis(300);

/// A `/dev/uinput`-backed [`OutputSink`].
pub struct UinputSink {
    device: VirtualDevice,
    /// Events buffered for the current frame, emitted together (with a
    /// trailing `SYN_REPORT`) on [`frame`](OutputSink::frame).
    pending: Vec<InputEvent>,
    /// First emit error since the last [`flush`](OutputSink::flush), surfaced
    /// there so the caller can react.
    error: Option<anyhow::Error>,
    /// Compositor output scale applied to absolute coordinates. libinput maps
    /// the device's `0..=ABS_MAX` range onto the output's physical pixels, but
    /// the cursor lives in logical (scaled) coordinates, so a full-surface
    /// sweep would overshoot by the scale factor (touching `1/scale` of the
    /// pad would already cover the whole screen). Dividing emitted absolute
    /// coordinates by the scale corrects this. `1.0` for relative devices and
    /// unscaled outputs, where it is a no-op.
    abs_scale: f64,
}

impl UinputSink {
    /// Build a relative pointer + full keyboard device (gamepad bridge).
    pub fn new_relative() -> Result<Self> {
        let mut rel = AttributeSet::<RelativeAxisCode>::new();
        rel.insert(RelativeAxisCode::REL_X);
        rel.insert(RelativeAxisCode::REL_Y);
        rel.insert(RelativeAxisCode::REL_WHEEL);
        rel.insert(RelativeAxisCode::REL_HWHEEL);

        let builder = VirtualDevice::builder()
            .context("open /dev/uinput (is the user allowed to write it?)")?;
        let device = builder
            .name("shepherd-bridge virtual pointer+keyboard")
            .with_keys(&keyboard_and_mouse_keys())
            .context("register uinput keys")?
            .with_relative_axes(&rel)
            .context("register uinput relative axes")?
            .build()
            .context("create uinput virtual device")?;

        Ok(Self::ready(device, 1.0))
    }

    /// Build an absolute pointer device (touch bridge).
    ///
    /// `output_scale` is the compositor's output scale (e.g. `1.5`); pass
    /// `1.0` when scaling is unknown or disabled. See [`UinputSink::abs_scale`].
    pub fn new_absolute(output_scale: f64) -> Result<Self> {
        let abs_info = AbsInfo::new(0, 0, ABS_MAX, 0, 0, 0);
        let abs_x = UinputAbsSetup::new(AbsoluteAxisCode::ABS_X, abs_info);
        let abs_y = UinputAbsSetup::new(AbsoluteAxisCode::ABS_Y, abs_info);

        let builder = VirtualDevice::builder()
            .context("open /dev/uinput (is the user allowed to write it?)")?;
        let device = builder
            .name("shepherd-bridge virtual absolute pointer")
            .with_keys(&mouse_buttons())
            .context("register uinput mouse buttons")?
            .with_absolute_axis(&abs_x)
            .context("register uinput ABS_X")?
            .with_absolute_axis(&abs_y)
            .context("register uinput ABS_Y")?
            .build()
            .context("create uinput virtual device")?;

        Ok(Self::ready(device, output_scale))
    }

    fn ready(device: VirtualDevice, abs_scale: f64) -> Self {
        // Give udev/the compositor a moment to bind the new node before any
        // events are emitted.
        thread::sleep(SETTLE);
        Self {
            device,
            pending: Vec::new(),
            error: None,
            abs_scale,
        }
    }
}

impl OutputSink for UinputSink {
    fn dispatch(&mut self, event: OutputEvent, _time: u32) {
        match event {
            OutputEvent::PointerMotion { dx, dy } => {
                let dx = dx.round() as i32;
                let dy = dy.round() as i32;
                if dx != 0 {
                    self.pending.push(InputEvent::new(EV_REL, REL_X, dx));
                }
                if dy != 0 {
                    self.pending.push(InputEvent::new(EV_REL, REL_Y, dy));
                }
            }
            OutputEvent::PointerMotionAbsolute {
                x,
                y,
                x_extent,
                y_extent,
            } => {
                let scale = self.abs_scale;
                self.pending.push(InputEvent::new(
                    EV_ABS,
                    ABS_X,
                    rescale_abs(x, x_extent, scale),
                ));
                self.pending.push(InputEvent::new(
                    EV_ABS,
                    ABS_Y,
                    rescale_abs(y, y_extent, scale),
                ));
            }
            OutputEvent::PointerButton { button, pressed } => {
                self.pending
                    .push(InputEvent::new(EV_KEY, button as u16, pressed as i32));
            }
            OutputEvent::PointerScroll { axis, discrete } => {
                // The preset layer uses Wayland's convention (positive =
                // down/right); evdev's wheel is positive = up, so vertical
                // scroll is negated. Horizontal already agrees (positive =
                // right).
                let (code, value) = match axis {
                    ScrollAxis::Vertical => (REL_WHEEL, -discrete),
                    ScrollAxis::Horizontal => (REL_HWHEEL, discrete),
                };
                self.pending.push(InputEvent::new(EV_REL, code, value));
            }
            OutputEvent::Key { keycode, pressed } => {
                self.pending
                    .push(InputEvent::new(EV_KEY, keycode as u16, pressed as i32));
            }
        }
    }

    fn frame(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        self.pending.push(InputEvent::new(EV_SYN, SYN_REPORT, 0));
        if let Err(e) = self.device.emit(&self.pending)
            && self.error.is_none()
        {
            self.error = Some(anyhow::Error::new(e).context("emit uinput events"));
        }
        self.pending.clear();
    }

    fn flush(&mut self) -> Result<()> {
        match self.error.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// Rescale a raw absolute coordinate in `0..=extent` into the device's
/// declared `0..=ABS_MAX` range, dividing by the compositor `scale` so the
/// mapped position lands in logical (scaled) coordinates rather than physical
/// pixels. At `scale == 1.0` this is the plain range remap. The result is
/// clamped to the declared range (relevant for downscaling, `scale < 1.0`).
fn rescale_abs(value: u32, extent: u32, scale: f64) -> i32 {
    if extent == 0 {
        return 0;
    }
    let scale = if scale > 0.0 { scale } else { 1.0 };
    let frac = f64::from(value.min(extent)) / f64::from(extent);
    let v = (frac / scale * f64::from(ABS_MAX)).round();
    v.clamp(0.0, f64::from(ABS_MAX)) as i32
}

/// The full set of keys a keyboard+mouse device may emit. We register
/// generously (all keyboard codes plus the mouse-button range) rather than
/// tracking exactly which codes a preset uses.
fn keyboard_and_mouse_keys() -> AttributeSet<KeyCode> {
    let mut keys = AttributeSet::<KeyCode>::new();
    for code in 1u16..=255 {
        keys.insert(KeyCode::new(code));
    }
    insert_mouse_buttons(&mut keys);
    keys
}

fn mouse_buttons() -> AttributeSet<KeyCode> {
    let mut keys = AttributeSet::<KeyCode>::new();
    insert_mouse_buttons(&mut keys);
    keys
}

fn insert_mouse_buttons(keys: &mut AttributeSet<KeyCode>) {
    for code in BTN_MOUSE_FIRST..=BTN_MOUSE_LAST {
        keys.insert(KeyCode::new(code));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert `a` and `b` are within 1 unit — the rounding in `rescale_abs`
    /// can land on either side of an exact half.
    fn near(a: i32, b: i32) {
        assert!((a - b).abs() <= 1, "{a} not within 1 of {b}");
    }

    #[test]
    fn rescale_maps_endpoints_and_midpoint() {
        assert_eq!(rescale_abs(0, 1000, 1.0), 0);
        assert_eq!(rescale_abs(1000, 1000, 1.0), ABS_MAX);
        near(rescale_abs(500, 1000, 1.0), ABS_MAX / 2);
    }

    #[test]
    fn rescale_clamps_and_guards_zero_extent() {
        assert_eq!(rescale_abs(2000, 1000, 1.0), ABS_MAX); // clamped to extent
        assert_eq!(rescale_abs(5, 0, 1.0), 0); // degenerate range
    }

    #[test]
    fn rescale_divides_by_scale() {
        // Under output scale, a full-surface sweep must reach only
        // `1/scale` of the device range so the cursor lands at the logical
        // (not physical) screen edge.
        near(rescale_abs(1000, 1000, 2.0), ABS_MAX / 2);
        near(
            rescale_abs(1000, 1000, 1.5),
            (f64::from(ABS_MAX) / 1.5) as i32,
        );
        near(rescale_abs(500, 1000, 2.0), ABS_MAX / 4);
    }

    #[test]
    fn rescale_downscale_clamps() {
        // scale < 1.0 would push a full sweep past the declared range; clamp.
        assert_eq!(rescale_abs(1000, 1000, 0.5), ABS_MAX);
    }

    #[test]
    fn rescale_nonpositive_scale_treated_as_one() {
        assert_eq!(rescale_abs(1000, 1000, 0.0), ABS_MAX);
    }
}

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

use std::collections::BTreeMap;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, EventType, InputEvent, KeyCode, PropType,
    RelativeAxisCode, UinputAbsSetup,
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
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const SYN_REPORT: u16 = 0x00;

// Mouse button range (BTN_LEFT..BTN_TASK).
const BTN_MOUSE_FIRST: u16 = 0x110;
const BTN_MOUSE_LAST: u16 = 0x117;
const BTN_TOUCH: u16 = 0x14a;

/// Logical resolution of the absolute pointer. Touch coordinates are rescaled
/// into `0..=ABS_MAX` so the device can declare a fixed range up front.
const ABS_MAX: i32 = 65535;

/// Highest multitouch slot the virtual touchscreen advertises (10 contacts).
const MAX_SLOT: i32 = 9;

/// How long to wait after creating the device for udev/libinput to bind it.
/// Events emitted before the compositor opens the node are silently dropped,
/// so the first real input would otherwise be lost.
const SETTLE: Duration = Duration::from_millis(300);

/// Whether `/dev/uinput` can be opened for writing.
///
/// Every synthetic input in Lunchbox goes through it — the touch and gamepad
/// bridges, and the HUD's page-turn buttons — and on a device where the node
/// is missing or not writable by the session user, all of them fail at the
/// moment someone tries to use them. Cheap enough to run from a diagnostics
/// sweep: an open and a close, no device created.
pub fn is_writable() -> bool {
    std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/uinput")
        .is_ok()
}

/// A `/dev/uinput`-backed [`OutputSink`].
pub struct UinputSink {
    device: VirtualDevice,
    /// Events buffered for the current frame, emitted together (with a
    /// trailing `SYN_REPORT`) on [`frame`](OutputSink::frame).
    pending: Vec<InputEvent>,
    /// First emit error since the last [`flush`](OutputSink::flush), surfaced
    /// there so the caller can react.
    error: Option<anyhow::Error>,
    /// Active multitouch contacts: slot → assigned tracking ID, for the MT
    /// type-B protocol on a touchscreen device. Empty for pointer/keyboard
    /// devices, which never emit `Touch*` events. Drives the `BTN_TOUCH`
    /// transitions (set on first contact, cleared when the last lifts).
    touch_tracking: BTreeMap<u32, i32>,
    /// Monotonic source of MT tracking IDs; each new contact gets a fresh one
    /// in `1..=u16::MAX` (the declared `ABS_MT_TRACKING_ID` range).
    next_tracking_id: i32,
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
            .name("lunchbox-bridge virtual pointer+keyboard")
            .with_keys(&keyboard_and_mouse_keys())
            .context("register uinput keys")?
            .with_relative_axes(&rel)
            .context("register uinput relative axes")?
            .build()
            .context("create uinput virtual device")?;

        Ok(Self::ready(device))
    }

    /// Build an absolute pointer device (touch bridge).
    ///
    /// The device declares a `0..=ABS_MAX` range that libinput maps onto the
    /// output's logical layout space, so a full-pad sweep already covers the
    /// whole screen 1:1 at any output scale — no per-scale correction is
    /// applied (see [`rescale_abs`]).
    pub fn new_absolute() -> Result<Self> {
        let abs_info = AbsInfo::new(0, 0, ABS_MAX, 0, 0, 0);
        let abs_x = UinputAbsSetup::new(AbsoluteAxisCode::ABS_X, abs_info);
        let abs_y = UinputAbsSetup::new(AbsoluteAxisCode::ABS_Y, abs_info);

        let builder = VirtualDevice::builder()
            .context("open /dev/uinput (is the user allowed to write it?)")?;
        let device = builder
            .name("lunchbox-bridge virtual absolute pointer")
            .with_keys(&mouse_buttons())
            .context("register uinput mouse buttons")?
            .with_absolute_axis(&abs_x)
            .context("register uinput ABS_X")?
            .with_absolute_axis(&abs_y)
            .context("register uinput ABS_Y")?
            .build()
            .context("create uinput virtual device")?;

        Ok(Self::ready(device))
    }

    /// Build a keyboard-only device (the HUD's page-turn buttons, issue #160).
    ///
    /// No pointer axes on purpose: a device that declares them makes libinput
    /// hand the seat a second pointer, and on a touch-only panel that means a
    /// mouse cursor appearing over the child's book the first time a button is
    /// pressed.
    pub fn new_keyboard() -> Result<Self> {
        let builder = VirtualDevice::builder()
            .context("open /dev/uinput (is the user allowed to write it?)")?;
        let device = builder
            .name("lunchbox-bridge virtual keyboard")
            .with_keys(&keyboard_keys())
            .context("register uinput keys")?
            .build()
            .context("create uinput virtual device")?;

        Ok(Self::ready(device))
    }

    /// Build a multitouch touchscreen device (tablet bridge).
    ///
    /// Emits the MT type-B protocol and declares `INPUT_PROP_DIRECT`, so
    /// libinput classifies the device as a touchscreen and the compositor
    /// delivers real `wl_touch` events to activities — not pointer events.
    /// Like [`UinputSink::new_absolute`], the declared range maps onto the
    /// output's logical space, so no per-scale correction is applied.
    pub fn new_touchscreen() -> Result<Self> {
        let abs_info = AbsInfo::new(0, 0, ABS_MAX, 0, 0, 0);
        let slot_info = AbsInfo::new(0, 0, MAX_SLOT, 0, 0, 0);
        let id_info = AbsInfo::new(0, 0, i32::from(u16::MAX), 0, 0, 0);

        let mut props = AttributeSet::<PropType>::new();
        props.insert(PropType::DIRECT);

        let mut keys = AttributeSet::<KeyCode>::new();
        keys.insert(KeyCode::BTN_TOUCH);

        let builder = VirtualDevice::builder()
            .context("open /dev/uinput (is the user allowed to write it?)")?;
        let device = builder
            .name("lunchbox-bridge virtual touchscreen")
            .with_properties(&props)
            .context("register uinput INPUT_PROP_DIRECT")?
            .with_keys(&keys)
            .context("register uinput BTN_TOUCH")?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_X, abs_info))
            .context("register uinput ABS_X")?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_Y, abs_info))
            .context("register uinput ABS_Y")?
            .with_absolute_axis(&UinputAbsSetup::new(
                AbsoluteAxisCode::ABS_MT_POSITION_X,
                abs_info,
            ))
            .context("register uinput ABS_MT_POSITION_X")?
            .with_absolute_axis(&UinputAbsSetup::new(
                AbsoluteAxisCode::ABS_MT_POSITION_Y,
                abs_info,
            ))
            .context("register uinput ABS_MT_POSITION_Y")?
            .with_absolute_axis(&UinputAbsSetup::new(
                AbsoluteAxisCode::ABS_MT_SLOT,
                slot_info,
            ))
            .context("register uinput ABS_MT_SLOT")?
            .with_absolute_axis(&UinputAbsSetup::new(
                AbsoluteAxisCode::ABS_MT_TRACKING_ID,
                id_info,
            ))
            .context("register uinput ABS_MT_TRACKING_ID")?
            .build()
            .context("create uinput virtual device")?;

        Ok(Self::ready(device))
    }

    fn ready(device: VirtualDevice) -> Self {
        // Give udev/the compositor a moment to bind the new node before any
        // events are emitted.
        thread::sleep(SETTLE);
        Self {
            device,
            pending: Vec::new(),
            error: None,
            touch_tracking: BTreeMap::new(),
            next_tracking_id: 0,
        }
    }

    /// Select the active MT slot for the events that follow in this frame.
    fn push_touch_slot(&mut self, slot: u32) {
        self.pending
            .push(InputEvent::new(EV_ABS, ABS_MT_SLOT, slot as i32));
    }

    /// Emit a contact position on both the MT axes and the single-touch
    /// `ABS_X`/`ABS_Y` axes (the latter keeps non-MT consumers in sync).
    fn push_touch_position(&mut self, x: u32, y: u32, x_extent: u32, y_extent: u32) {
        let rx = rescale_abs(x, x_extent);
        let ry = rescale_abs(y, y_extent);
        self.pending
            .push(InputEvent::new(EV_ABS, ABS_MT_POSITION_X, rx));
        self.pending
            .push(InputEvent::new(EV_ABS, ABS_MT_POSITION_Y, ry));
        self.pending.push(InputEvent::new(EV_ABS, ABS_X, rx));
        self.pending.push(InputEvent::new(EV_ABS, ABS_Y, ry));
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
                self.pending
                    .push(InputEvent::new(EV_ABS, ABS_X, rescale_abs(x, x_extent)));
                self.pending
                    .push(InputEvent::new(EV_ABS, ABS_Y, rescale_abs(y, y_extent)));
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
            OutputEvent::TouchDown {
                slot,
                x,
                y,
                x_extent,
                y_extent,
            } => {
                let first_contact = self.touch_tracking.is_empty();
                self.next_tracking_id = self.next_tracking_id % i32::from(u16::MAX) + 1;
                let id = self.next_tracking_id;
                self.touch_tracking.insert(slot, id);
                self.push_touch_slot(slot);
                self.pending
                    .push(InputEvent::new(EV_ABS, ABS_MT_TRACKING_ID, id));
                self.push_touch_position(x, y, x_extent, y_extent);
                if first_contact {
                    self.pending.push(InputEvent::new(EV_KEY, BTN_TOUCH, 1));
                }
            }
            OutputEvent::TouchMotion {
                slot,
                x,
                y,
                x_extent,
                y_extent,
            } => {
                // Ignore motion for a contact we never saw go down; the MT
                // protocol requires a live tracking ID in the slot first.
                if self.touch_tracking.contains_key(&slot) {
                    self.push_touch_slot(slot);
                    self.push_touch_position(x, y, x_extent, y_extent);
                }
            }
            OutputEvent::TouchUp { slot } => {
                if self.touch_tracking.remove(&slot).is_some() {
                    self.push_touch_slot(slot);
                    self.pending
                        .push(InputEvent::new(EV_ABS, ABS_MT_TRACKING_ID, -1));
                    if self.touch_tracking.is_empty() {
                        self.pending.push(InputEvent::new(EV_KEY, BTN_TOUCH, 0));
                    }
                }
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
/// declared `0..=ABS_MAX` range. libinput maps that range onto the output's
/// logical layout space, so a full-extent sweep lands 1:1 on the screen at any
/// output scale — no scale correction is applied here. (An earlier version
/// divided by the compositor scale (issue #58); that was wrong in principle —
/// it only appeared correct because it was only ever run with the XWayland
/// HiDPI workaround forcing scale to 1.0, where the divide is a no-op — and it
/// caused the touch-compat offset in issue #47 on scaled outputs.)
fn rescale_abs(value: u32, extent: u32) -> i32 {
    if extent == 0 {
        return 0;
    }
    let frac = f64::from(value.min(extent)) / f64::from(extent);
    let v = (frac * f64::from(ABS_MAX)).round();
    v.clamp(0.0, f64::from(ABS_MAX)) as i32
}

/// The full set of keys a keyboard+mouse device may emit. We register
/// generously (all keyboard codes plus the mouse-button range) rather than
/// tracking exactly which codes a preset uses.
fn keyboard_and_mouse_keys() -> AttributeSet<KeyCode> {
    let mut keys = keyboard_keys();
    insert_mouse_buttons(&mut keys);
    keys
}

fn keyboard_keys() -> AttributeSet<KeyCode> {
    let mut keys = AttributeSet::<KeyCode>::new();
    for code in 1u16..=255 {
        keys.insert(KeyCode::new(code));
    }
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
        // A full-extent sweep maps 1:1 onto the declared range at every scale;
        // libinput maps that range onto the output's logical space (issue #47).
        assert_eq!(rescale_abs(0, 1000), 0);
        assert_eq!(rescale_abs(1000, 1000), ABS_MAX);
        near(rescale_abs(500, 1000), ABS_MAX / 2);
    }

    #[test]
    fn rescale_clamps_and_guards_zero_extent() {
        assert_eq!(rescale_abs(2000, 1000), ABS_MAX); // clamped to extent
        assert_eq!(rescale_abs(5, 0), 0); // degenerate range
    }
}

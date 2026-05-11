//! Wayland-side plumbing: connection, registry binding, virtual pointer +
//! virtual keyboard creation, and helpers that turn `OutputEvent`s into
//! protocol calls.

use std::fs::File;
use std::io::Write;
use std::os::fd::AsFd;

use anyhow::{Context, Result, anyhow};
use nix::sys::memfd::{MemFdCreateFlag, memfd_create};
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle,
    protocol::{wl_pointer, wl_registry, wl_seat},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

use crate::preset::{OutputEvent, ScrollAxis};

/// A minimal xkb keymap that lets the compositor resolve symbols via its own
/// xkb data files. This avoids pulling in libxkbcommon — the bridge just
/// ships evdev keycodes, and the compositor's libxkbcommon handles the
/// rest.
const KEYMAP: &str = "xkb_keymap {
    xkb_keycodes  \"evdev\"    { include \"evdev\" };
    xkb_types     \"complete\" { include \"complete\" };
    xkb_compat    \"complete\" { include \"complete\" };
    xkb_symbols   \"us\"       { include \"pc+us\" };
};
";

#[derive(Default)]
pub struct WaylandState {
    pub seat: Option<wl_seat::WlSeat>,
    pub pointer_manager: Option<ZwlrVirtualPointerManagerV1>,
    pub keyboard_manager: Option<ZwpVirtualKeyboardManagerV1>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for WaylandState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_seat" => {
                    let seat = registry.bind::<wl_seat::WlSeat, _, _>(name, version.min(7), qh, ());
                    state.seat = Some(seat);
                }
                "zwlr_virtual_pointer_manager_v1" => {
                    let manager = registry.bind::<ZwlrVirtualPointerManagerV1, _, _>(
                        name,
                        version.min(2),
                        qh,
                        (),
                    );
                    state.pointer_manager = Some(manager);
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    let manager = registry.bind::<ZwpVirtualKeyboardManagerV1, _, _>(
                        name,
                        version.min(1),
                        qh,
                        (),
                    );
                    state.keyboard_manager = Some(manager);
                }
                _ => {}
            }
        }
    }
}

macro_rules! noop_dispatch {
    ($t:ty) => {
        impl Dispatch<$t, ()> for WaylandState {
            fn event(
                _: &mut Self,
                _: &$t,
                _: <$t as wayland_client::Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    };
}

noop_dispatch!(wl_seat::WlSeat);
noop_dispatch!(ZwlrVirtualPointerManagerV1);
noop_dispatch!(ZwlrVirtualPointerV1);
noop_dispatch!(ZwpVirtualKeyboardManagerV1);
noop_dispatch!(ZwpVirtualKeyboardV1);

/// Output endpoints used by `dispatch_event`. The keyboard is optional —
/// some compositors might not expose the virtual-keyboard protocol, in
/// which case keyboard events are silently dropped.
pub struct WaylandOutputs {
    // The Connection must outlive the queue/pointer/keyboard it owns —
    // dropping it would close the socket and invalidate every proxy.
    _conn: Connection,
    event_queue: EventQueue<WaylandState>,
    state: WaylandState,
    pointer: ZwlrVirtualPointerV1,
    keyboard: Option<ZwpVirtualKeyboardV1>,
}

impl WaylandOutputs {
    pub fn connect() -> Result<Self> {
        let conn = Connection::connect_to_env().context("failed to connect to Wayland display")?;
        let display = conn.display();
        let mut event_queue = conn.new_event_queue::<WaylandState>();
        let qh = event_queue.handle();
        let _registry = display.get_registry(&qh, ());

        let mut state = WaylandState::default();
        event_queue
            .roundtrip(&mut state)
            .context("Wayland registry roundtrip failed")?;

        let pointer_manager = state
            .pointer_manager
            .clone()
            .ok_or_else(|| anyhow!("compositor does not support zwlr_virtual_pointer_v1"))?;
        let pointer = pointer_manager.create_virtual_pointer(state.seat.as_ref(), &qh, ());

        // Virtual keyboard is best-effort. If the compositor doesn't expose
        // it (older wlroots, non-wlroots compositors that ship only the
        // pointer), continue with mouse-only output and warn.
        let keyboard = if let (Some(km), Some(seat)) =
            (state.keyboard_manager.clone(), state.seat.clone())
        {
            let kb = km.create_virtual_keyboard(&seat, &qh, ());
            upload_keymap(&kb)?;
            Some(kb)
        } else {
            tracing::warn!(
                "compositor does not support zwp_virtual_keyboard_v1; gamepad → keyboard events will be dropped"
            );
            None
        };

        event_queue.flush()?;
        Ok(Self {
            _conn: conn,
            event_queue,
            state,
            pointer,
            keyboard,
        })
    }

    pub fn flush(&mut self) -> Result<()> {
        self.event_queue.flush()?;
        self.event_queue
            .dispatch_pending(&mut self.state)
            .context("Wayland dispatch failed")?;
        Ok(())
    }

    /// Translate one `OutputEvent` into Wayland protocol calls. Callers
    /// should `frame()` after each logical batch of events.
    pub fn dispatch(&self, event: OutputEvent, time: u32) {
        match event {
            OutputEvent::PointerMotion { dx, dy } => {
                self.pointer.motion(time, dx as f64, dy as f64);
            }
            OutputEvent::PointerButton { button, pressed } => {
                let state = if pressed {
                    wl_pointer::ButtonState::Pressed
                } else {
                    wl_pointer::ButtonState::Released
                };
                self.pointer.button(time, button, state);
            }
            OutputEvent::PointerScroll { axis, discrete } => {
                let wl_axis = match axis {
                    ScrollAxis::Vertical => wl_pointer::Axis::VerticalScroll,
                    ScrollAxis::Horizontal => wl_pointer::Axis::HorizontalScroll,
                };
                // Each notch corresponds to ~15 logical pixels of scroll, the
                // de-facto convention compositors expect alongside
                // axis_discrete.
                let value = discrete as f64 * 15.0;
                self.pointer.axis(time, wl_axis, value);
                self.pointer.axis_discrete(time, wl_axis, value, discrete);
            }
            OutputEvent::Key { keycode, pressed } => {
                if let Some(kb) = &self.keyboard {
                    kb.key(time, keycode, if pressed { 1 } else { 0 });
                }
            }
        }
    }

    pub fn frame(&self) {
        self.pointer.frame();
    }

    pub fn destroy(self) {
        self.pointer.destroy();
        if let Some(kb) = self.keyboard {
            kb.destroy();
        }
    }
}

fn upload_keymap(kb: &ZwpVirtualKeyboardV1) -> Result<()> {
    let name = c"shepherd-gamepad-bridge-keymap";
    let fd = memfd_create(name, MemFdCreateFlag::MFD_CLOEXEC).context("memfd_create failed")?;
    let mut file = File::from(fd);
    file.write_all(KEYMAP.as_bytes())
        .context("failed to write keymap to memfd")?;
    file.flush().ok();
    // Format 1 = xkb_v1 text. The compositor will mmap the fd and parse it.
    kb.keymap(1, file.as_fd(), KEYMAP.len() as u32);
    Ok(())
}

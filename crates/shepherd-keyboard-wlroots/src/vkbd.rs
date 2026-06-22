//! Virtual keyboard: emits real keysyms (Enter / Backspace / Tab) that text commit can't
//! express. `input-method-v2` handles text; this handles keys.
//!
//! wayland-protocols-misc has no high-level wrapper for `zwp_virtual_keyboard_v1`, so this
//! binds it directly. The interface has no events, so its `Dispatch` impls (in `main.rs`)
//! are empty.

use std::fs::File;
use std::io::Write;
use std::os::fd::AsFd;

use anyhow::Context as _;
use shepherd_keyboard_core::KeySym;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Dispatch, QueueHandle};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};

/// Linux/evdev keycodes (the codes a standard xkb keymap expects from `key`).
const KEY_BACKSPACE: u32 = 14;
const KEY_TAB: u32 = 15;
const KEY_ENTER: u32 = 28;

/// xkb keymap format `XKB_KEYMAP_FORMAT_TEXT_V1`.
const KEYMAP_FORMAT_TEXT_V1: u32 = 1;
const KEY_STATE_RELEASED: u32 = 0;
const KEY_STATE_PRESSED: u32 = 1;

fn evdev_code(sym: KeySym) -> u32 {
    match sym {
        KeySym::Backspace => KEY_BACKSPACE,
        KeySym::Tab => KEY_TAB,
        KeySym::Enter => KEY_ENTER,
    }
}

/// A bound virtual keyboard with a US keymap uploaded.
pub struct VirtualKeyboard {
    vk: ZwpVirtualKeyboardV1,
    // Keep the keymap fd's backing file alive for the program's lifetime so the compositor
    // can mmap it whenever it processes the `keymap` request.
    _keymap_file: File,
    time: u32,
}

impl VirtualKeyboard {
    /// Create a virtual keyboard on `seat`, upload a US xkb keymap, and zero the modifiers.
    pub fn new<D>(
        manager: &ZwpVirtualKeyboardManagerV1,
        seat: &WlSeat,
        qh: &QueueHandle<D>,
    ) -> anyhow::Result<Self>
    where
        D: Dispatch<ZwpVirtualKeyboardV1, ()> + 'static,
    {
        let vk = manager.create_virtual_keyboard(seat, qh, ());
        let keymap_file = upload_us_keymap(&vk)?;
        vk.modifiers(0, 0, 0, 0);
        Ok(Self {
            vk,
            _keymap_file: keymap_file,
            time: 0,
        })
    }

    /// Emit a single press+release of `sym`.
    pub fn key(&mut self, sym: KeySym) {
        let code = evdev_code(sym);
        self.time = self.time.wrapping_add(1);
        self.vk.key(self.time, code, KEY_STATE_PRESSED);
        self.time = self.time.wrapping_add(1);
        self.vk.key(self.time, code, KEY_STATE_RELEASED);
    }
}

/// Compile a US xkb keymap, write it to a temp file, and hand the fd to the compositor.
fn upload_us_keymap(vk: &ZwpVirtualKeyboardV1) -> anyhow::Result<File> {
    use xkbcommon::xkb;

    let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap =
        xkb::Keymap::new_from_names(&ctx, "", "", "us", "", None, xkb::KEYMAP_COMPILE_NO_FLAGS)
            .context("failed to compile US xkb keymap")?;
    let text = keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);

    let mut file = tempfile::tempfile().context("create keymap temp file")?;
    file.write_all(text.as_bytes())?;
    file.write_all(&[0])?; // keymap must be NUL-terminated
    file.flush()?;
    let size = text.len() as u32 + 1;

    vk.keymap(KEYMAP_FORMAT_TEXT_V1, file.as_fd(), size);
    Ok(file)
}

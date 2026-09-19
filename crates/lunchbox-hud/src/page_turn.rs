//! Page-turn keys for reading activities (issue #160).
//!
//! A document reader turns pages on a keystroke, a gamepad D-pad or a scroll
//! wheel. A touchscreen produces none of those, and Okular — the reader behind
//! `type = "ebook"` entries — grabs only the pinch gesture and has no
//! swipe-to-turn. On a touch-only panel that leaves a child stranded on page
//! one.
//!
//! The HUD is the natural place to fix that: it is already on screen, on the
//! overlay layer, and it is shepherd's own surface rather than something the
//! reader's own restrictions could take away. So the buttons live there, and
//! pressing one synthesizes the key the reader is already listening for.
//!
//! The key goes out through a `/dev/uinput` device, the same backend the input
//! bridges use ([`lunchbox_bridge`]) — the compositor picks it up through
//! libinput and delivers it to whatever holds keyboard focus. That is the
//! activity: the HUD's bar is a layer surface with `KeyboardMode::None`, so
//! pressing one of its buttons never takes focus away from the book.

use std::cell::RefCell;
use std::rc::Rc;

use lunchbox_bridge::{OutputEvent, OutputSink, UinputSink};
use tracing::{debug, warn};

/// `KEY_PAGEDOWN` / `KEY_PAGEUP`, the evdev codes for the keys every reader
/// binds to "next page" / "previous page". They are also what a gamepad
/// bridge's D-pad emits, so a reader that responds to one responds to both.
const KEY_PAGEDOWN: u32 = 109;
const KEY_PAGEUP: u32 = 104;

/// Which way a page-turn button goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Back,
}

impl Direction {
    fn keycode(self) -> u32 {
        match self {
            Direction::Forward => KEY_PAGEDOWN,
            Direction::Back => KEY_PAGEUP,
        }
    }
}

/// A lazily-created virtual keyboard that only ever sends page keys.
///
/// Lazy because most sessions are not reading sessions, and creating the
/// device costs a `/dev/uinput` handle plus a settle delay while libinput
/// binds it. It is created on the first press and kept for the life of the
/// HUD: re-creating it per press would pay that delay every time, and the
/// first key after a fresh device is the one libinput is most likely to drop.
pub struct PageTurner {
    sink: Option<UinputSink>,
    /// Set once the device could not be created, so a device this box will
    /// never allow is not retried on every press.
    unavailable: bool,
}

impl PageTurner {
    pub fn new() -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self {
            sink: None,
            unavailable: false,
        }))
    }

    /// Send one press-and-release. Returns whether it went out.
    ///
    /// A failure here is worth a log line and nothing else: the button is a
    /// convenience on a device that may have a keyboard anyway, and a HUD that
    /// died because `/dev/uinput` was not writable would take the clock, the
    /// volume control and the end-session button with it.
    pub fn turn(&mut self, direction: Direction) -> bool {
        if self.unavailable {
            return false;
        }
        if self.sink.is_none() {
            match UinputSink::new_keyboard() {
                Ok(sink) => self.sink = Some(sink),
                Err(e) => {
                    warn!(
                        error = %e,
                        "Could not create the virtual keyboard for page turning; \
                         the HUD's page buttons will do nothing"
                    );
                    self.unavailable = true;
                    return false;
                }
            }
        }

        let Some(sink) = self.sink.as_mut() else {
            return false;
        };
        let keycode = direction.keycode();
        sink.dispatch(
            OutputEvent::Key {
                keycode,
                pressed: true,
            },
            0,
        );
        sink.frame();
        sink.dispatch(
            OutputEvent::Key {
                keycode,
                pressed: false,
            },
            0,
        );
        sink.frame();
        match sink.flush() {
            Ok(()) => {
                debug!(?direction, keycode, "Sent a page-turn key");
                true
            }
            Err(e) => {
                warn!(error = %e, "Page-turn key could not be delivered");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The codes are the contract with every reader, and a transposed pair
    /// would send a child backwards through the book.
    #[test]
    fn directions_map_to_the_page_keys() {
        assert_eq!(Direction::Forward.keycode(), 109); // KEY_PAGEDOWN
        assert_eq!(Direction::Back.keycode(), 104); // KEY_PAGEUP
    }

    /// A box without `/dev/uinput` must degrade to a dead button, not a dead
    /// HUD — and must not retry on every press.
    #[test]
    fn an_unavailable_device_is_not_retried() {
        let turner = PageTurner::new();
        let mut turner = turner.borrow_mut();
        turner.unavailable = true;
        assert!(!turner.turn(Direction::Forward));
        assert!(turner.sink.is_none());
    }
}

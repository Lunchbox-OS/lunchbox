// Shepherd Swipe Keyboard — GNOME Shell extension (SCAFFOLD).
//
// GNOME exposes neither input-method-v2, virtual-keyboard, nor layer-shell, so the GNOME
// backend is this Shell extension plus the Rust decode daemon (shepherd-keyboard-gnome-daemon):
// the extension renders the keyboard, captures touch, reads input purpose + surrounding text,
// and commits text through GNOME's own input-method object; it calls the daemon over D-Bus
// only for candidates, so GNOME and wlroots decode through the identical core path.
//
// STATUS: the D-Bus wiring to the daemon and the safety-gate decision below are concrete and
// reviewable. The Shell-specific glue (suppressing the built-in OSK, rendering the keyboard
// actor, capturing touch on it, and committing through the IM object) is marked `TODO(shell)`
// and MUST be completed and verified on the target GNOME Shell version — GJS/Shell APIs drift
// across releases. See README.md for the manual verification checklist (the Phase 4 gate).

import Gio from 'gi://Gio';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const BUS_NAME = 'com.armeafamily.ShepherdSwipe';
const BUS_PATH = '/com/armeafamily/ShepherdSwipe';

// Matches the daemon's interface (com.armeafamily.ShepherdSwipe1).
const DECODER_IFACE = `
<node>
  <interface name="com.armeafamily.ShepherdSwipe1">
    <method name="Decode">
      <arg type="s" direction="in" name="gesture_json"/>
      <arg type="s" direction="in" name="preceding_text"/>
      <arg type="a(sd)" direction="out" name="candidates"/>
    </method>
    <property name="Available" type="b" access="read"/>
    <property name="Profile" type="s" access="read"/>
  </interface>
</node>`;

const DecoderProxy = Gio.DBusProxy.makeProxyWrapper(DECODER_IFACE);

// GNOME input purposes that force plain tap-only entry (mirrors the Rust core's safety gate:
// no swipe, no suggestions, no surrounding-text reads). Keep in lockstep with
// shepherd-keyboard-core::safety. St/Clutter expose these as Gtk.InputPurpose values.
const SENSITIVE_PURPOSES = new Set(['password', 'pin']);

export default class ShepherdSwipeExtension extends Extension {
    enable() {
        this._proxy = DecoderProxy(Gio.DBus.session, BUS_NAME, BUS_PATH);

        // TODO(shell): suppress GNOME's built-in OSK while this extension is active
        // (Main.keyboard) so the two keyboards don't both appear.
        // TODO(shell): build the keyboard actor (St.Widget grid + suggestion bar) and add it
        // to the Shell, shown on text-field focus / touch and hidden on blur.
        // TODO(shell): capture touch on the actor, normalize points against the rendered key
        // area, and assemble a v1 gesture.json (same coordinate convention as the core's
        // GestureBuilder), then call `this.decode(...)`.
        // TODO(shell): commit candidates / taps through GNOME's input-method object (the same
        // channel the built-in OSK uses — no IBus engine, no uinput), honoring `this.gate()`.
        console.log('shepherd-swipe: enabled (scaffold); daemon proxy created');
    }

    disable() {
        // TODO(shell): tear down the actor, restore the built-in OSK, disconnect focus signals.
        this._proxy = null;
    }

    /// Safety gate (host spec §4.4): in a sensitive field, do not swipe, do not show
    /// suggestions, and do not read surrounding text. The caller MUST honor `useSurrounding`
    /// before reading any surrounding text from the IM object.
    ///
    /// `purpose` is the focused field's input purpose (lowercased name) and `sensitiveHint`
    /// the "sensitive data" content hint, both read from the Shell input-method object.
    gate(purpose, sensitiveHint) {
        const tapOnly = SENSITIVE_PURPOSES.has(purpose) || !!sensitiveHint;
        return {tapOnly, swipe: !tapOnly, suggestions: !tapOnly, useSurrounding: !tapOnly};
    }

    /// Decode a gesture (v1 gesture.json) into ranked [word, score] pairs via the daemon.
    /// `precedingText` MUST be '' in a sensitive field (see `gate`). Returns [] on error so
    /// the caller falls back to letter entry.
    async decode(gestureJson, precedingText) {
        if (!this._proxy)
            return [];
        try {
            const [candidates] = await this._proxy.DecodeAsync(gestureJson, precedingText ?? '');
            return candidates; // array of [word, score]
        } catch (e) {
            console.warn(`shepherd-swipe: decode failed: ${e}`);
            return [];
        }
    }
}

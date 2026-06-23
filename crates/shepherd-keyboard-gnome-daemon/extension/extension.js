// Shepherd Swipe Keyboard — GNOME Shell extension (GNOME 50).
//
// The GNOME half of the swipe keyboard. GNOME exposes neither input-method-v2,
// virtual-keyboard, nor layer-shell, so this Shell extension renders the keyboard, captures
// touch, reads input purpose + surrounding text, and commits text through GNOME's own
// input-method object (Main.inputMethod); it calls the Rust decode daemon
// (shepherd-keyboard-gnome-daemon) over D-Bus only for candidates, so GNOME and wlroots
// decode through the identical core path.
//
// API surface confirmed empirically on GNOME Shell 50 (see the daemon README):
//   - Main.inputMethod.commit(text)            commit text into the focused field
//   - Main.inputMethod.handleVirtualKey(keyval) emit Enter/Backspace/Tab
//   - Main.inputMethod.getSurroundingText()     preceding text (skipped in sensitive fields)
//   - Main.inputMethod._purpose / ._hints       focused field purpose/hints (gate input)
//   - Main.layoutManager.keyboardBox            the OSK slot (reflows apps)
//   - Main.keyboard.open                        overridden to suppress the built-in OSK

import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';
import St from 'gi://St';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

import {
    LAYOUT, arcLength, gate, makeDecoderProxy, nearestKey, toGestureJson,
} from './decoder.js';

const KEYBOARD_HEIGHT = 320;
const SUGGESTION_FRACTION = 0.18;
const FUNCTION_FRACTION = 0.24;
const TAP_MAX_PATH = 0.12; // normalized; matches the core's GestureBuilder
const SUGGESTION_SLOTS = 4;

export default class ShepherdSwipeExtension extends Extension {
    enable() {
        this._proxy = makeDecoderProxy();
        this._shift = false;
        this._gesture = null; // {points: [{x,y,t}], t0}
        this._visible = false;
        this._focusIds = [];

        this._buildKeyboard();
        this._suppressBuiltinOsk();
        this._connectFocus();
        this._syncVisibility(); // hidden unless a field is already focused

        if (GLib.getenv('SHEPHERD_SWIPE_SELFTEST'))
            this._selfTest();
    }

    disable() {
        this._restoreBuiltinOsk();
        for (const [obj, id] of this._focusIds ?? [])
            obj.disconnect(id);
        this._focusIds = [];
        if (this._root) {
            Main.layoutManager.removeChrome(this._root);
            this._root.destroy();
            this._root = null;
        }
        this._keyArea = null;
        this._suggestionButtons = null;
        this._proxy = null;
        this._gesture = null;
    }

    // --- UI ---------------------------------------------------------------------------

    _buildKeyboard() {
        const monitor = Main.layoutManager.primaryMonitor;
        const width = monitor ? monitor.width : 1080;
        const height = KEYBOARD_HEIGHT;
        const sugH = Math.round(height * SUGGESTION_FRACTION);
        const fnH = Math.round(height * FUNCTION_FRACTION);
        const keyH = height - sugH - fnH;

        this._root = new St.BoxLayout({
            vertical: true,
            style_class: 'shepherd-swipe-keyboard',
            width,
            height,
        });

        // Suggestion bar.
        const bar = new St.BoxLayout({width, height: sugH});
        this._suggestionButtons = [];
        for (let i = 0; i < SUGGESTION_SLOTS; i++) {
            const b = new St.Button({
                style_class: 'shepherd-swipe-suggestion',
                label: '',
                x_expand: true,
            });
            b.connect('clicked', () => this._onSuggestion(i));
            bar.add_child(b);
            this._suggestionButtons.push(b);
        }
        this._root.add_child(bar);

        // Letter grid (absolute positioning from normalized geometry).
        this._keyArea = new St.Widget({
            width,
            height: keyH,
            reactive: true,
            layout_manager: new Clutter.FixedLayout(),
        });
        const kw = LAYOUT.keyWidth * width;
        const kh = LAYOUT.keyHeight * keyH;
        for (const key of LAYOUT.keys) {
            const btn = new St.Button({style_class: 'shepherd-swipe-key', label: key.l});
            this._keyArea.add_child(btn);
            btn.set_size(Math.round(kw * 0.92), Math.round(kh * 0.92));
            btn.set_position(
                Math.round(key.x * width - kw / 2),
                Math.round(key.y * keyH - kh / 2)
            );
            btn.connect('clicked', () => this._commitChar(key.l));
        }
        this._wireSwipeCapture(width, keyH);
        this._root.add_child(this._keyArea);

        // Function row.
        const fnRow = new St.BoxLayout({width, height: fnH});
        const fnKeys = [
            ['Shift', () => (this._shift = !this._shift)],
            ['123', () => {}],
            ['space', () => this._commit(' '), 3],
            ['⌫', () => this._key(Clutter.KEY_BackSpace)],
            ['↵', () => this._key(Clutter.KEY_Return)],
        ];
        for (const [label, fn, expand = 1] of fnKeys) {
            const b = new St.Button({style_class: 'shepherd-swipe-key', label, x_expand: true});
            b.set_width(Math.round((width / 7) * expand));
            b.connect('clicked', () => fn());
            fnRow.add_child(b);
        }
        this._root.add_child(fnRow);

        // Self-managed bottom-docked surface (reserves space via struts), shown on text
        // focus. Using addChrome rather than keyboardBox keeps full control of visibility.
        this._root.hide();
        Main.layoutManager.addChrome(this._root, {
            affectsStruts: true,
            trackFullscreen: true,
        });
        this._positionAtBottom();
    }

    _positionAtBottom() {
        const m = Main.layoutManager.primaryMonitor;
        if (m && this._root)
            this._root.set_position(m.x, m.y + m.height - this._root.height);
    }

    // --- focus-driven show / hide -----------------------------------------------------

    _connectFocus() {
        const im = Main.inputMethod;
        const sync = () => this._syncVisibility();
        // cursor-location-changed / surrounding-text-set fire when an editable is focused or
        // updated; notify::focus-window catches window switches and closes (blur). currentFocus
        // is the source of truth (no GObject focus signal exists on GNOME 50).
        this._focusIds.push([im, im.connect('cursor-location-changed', sync)]);
        this._focusIds.push([im, im.connect('surrounding-text-set', sync)]);
        this._focusIds.push([
            global.display,
            global.display.connect('notify::focus-window', sync),
        ]);
    }

    _syncVisibility() {
        if (Main.inputMethod?.currentFocus != null)
            this._showKeyboard();
        else
            this._hideKeyboard();
    }

    _showKeyboard() {
        if (!this._root)
            return;
        this._positionAtBottom();
        this._root.show(); // always assert visibility (addChrome can re-show on its own)
        this._visible = true;
    }

    _hideKeyboard() {
        if (!this._root)
            return;
        this._root.hide();
        if (this._visible) {
            // Side-effects only on a real show→hide transition.
            this._showSuggestions([]);
            this._gesture = null;
        }
        this._visible = false;
    }

    _wireSwipeCapture(width, keyH) {
        const norm = event => {
            const [sx, sy] = event.get_coords();
            const [ax, ay] = this._keyArea.get_transformed_position();
            return {x: (sx - ax) / width, y: (sy - ay) / keyH, t: event.get_time()};
        };
        const begin = event => {
            const p = norm(event);
            this._gesture = {points: [p], t0: p.t};
            return Clutter.EVENT_PROPAGATE; // let St.Button still receive a plain tap
        };
        const extend = event => {
            if (!this._gesture)
                return Clutter.EVENT_PROPAGATE;
            const p = norm(event);
            this._gesture.points.push({x: p.x, y: p.y, t: p.t - this._gesture.t0});
            return Clutter.EVENT_PROPAGATE;
        };
        const finish = () => {
            const g = this._gesture;
            this._gesture = null;
            if (g)
                this._onStrokeEnd(g.points);
            return Clutter.EVENT_PROPAGATE;
        };
        this._keyArea.connect('button-press-event', (_a, e) => begin(e));
        this._keyArea.connect('motion-event', (_a, e) => extend(e));
        this._keyArea.connect('button-release-event', () => finish());
        this._keyArea.connect('touch-event', (_a, e) => {
            switch (e.type()) {
            case Clutter.EventType.TOUCH_BEGIN: return begin(e);
            case Clutter.EventType.TOUCH_UPDATE: return extend(e);
            case Clutter.EventType.TOUCH_END: return finish();
            }
            return Clutter.EVENT_PROPAGATE;
        });
    }

    // --- input handling ---------------------------------------------------------------

    /** Current safety gate from the focused field's purpose/hints. */
    _gate() {
        const im = Main.inputMethod;
        return gate(im?._purpose ?? 0, im?._hints ?? 0);
    }

    _onStrokeEnd(points) {
        // A short path is a tap; the St.Button 'clicked' already handled the letter, so only
        // act on genuine swipes here.
        if (points.length < 2 || arcLength(points) < TAP_MAX_PATH)
            return;
        const g = this._gate();
        if (!g.swipe)
            return; // sensitive field: tap only, no decode
        const preceding = g.useSurrounding ? this._precedingText() : '';
        this._proxy.DecodeRemote(toGestureJson(points), preceding, ([candidates]) => {
            this._showSuggestions((candidates ?? []).map(c => c[0]));
        });
    }

    _precedingText() {
        try {
            const r = Main.inputMethod.getSurroundingText();
            // GNOME returns [text, cursor, anchor]; preceding = text up to cursor.
            if (r && r.length >= 2 && typeof r[0] === 'string')
                return r[0].slice(0, r[1] | 0);
        } catch (_e) {}
        return '';
    }

    _showSuggestions(words) {
        this._suggestions = words;
        for (let i = 0; i < this._suggestionButtons.length; i++)
            this._suggestionButtons[i].label = words[i] ?? '';
    }

    _onSuggestion(index) {
        const word = this._suggestions?.[index];
        if (word) {
            this._commit(`${word} `);
            this._showSuggestions([]);
        }
    }

    _commitChar(ch) {
        this._commit(this._shift ? ch.toUpperCase() : ch);
        this._shift = false;
        this._showSuggestions([]);
    }

    _commit(text) {
        try {
            Main.inputMethod.commit(text);
        } catch (e) {
            console.warn(`shepherd-swipe: commit failed: ${e}`);
        }
    }

    _key(keyval) {
        try {
            Main.inputMethod.handleVirtualKey(keyval);
        } catch (e) {
            console.warn(`shepherd-swipe: handleVirtualKey failed: ${e}`);
        }
    }

    // --- built-in OSK suppression -----------------------------------------------------

    _suppressBuiltinOsk() {
        try {
            this._savedOpen = Main.keyboard.open;
            Main.keyboard.open = () => {};
        } catch (_e) {}
    }

    _restoreBuiltinOsk() {
        try {
            if (this._savedOpen)
                Main.keyboard.open = this._savedOpen;
        } catch (_e) {}
        this._savedOpen = null;
    }

    // --- dev self-test (SHEPHERD_SWIPE_SELFTEST=1) -------------------------------------

    _selfTest() {
        const T = (name, ok, detail = '') =>
            console.log(`SHEPHERD-SELFTEST ${ok ? 'PASS' : 'FAIL'} ${name} ${detail}`);
        T('commit-is-fn', typeof Main.inputMethod?.commit === 'function');
        T('handleVirtualKey-is-fn', typeof Main.inputMethod?.handleVirtualKey === 'function');
        T('actor-built', !!this._root && this._keyArea.get_n_children() === LAYOUT.keys.length,
            `keys=${this._keyArea?.get_n_children()}`);
        T('on-stage', this._root?.get_stage() !== null);

        // Focus-driven show/hide: starts hidden, and the show/hide mechanics work. (A real
        // focus-in can't be simulated headless, so drive the handlers directly.)
        T('focus-signals-connected', this._focusIds.length === 3, `n=${this._focusIds.length}`);
        // No app is focused in the headless harness, so the keyboard must be hidden after the
        // initial sync; then verify the show/hide mechanics directly.
        T('starts-hidden', !this._root.visible);
        this._showKeyboard();
        const shown = this._root.visible;
        this._hideKeyboard();
        T('show-then-hide', shown && !this._root.visible);

        const normalGate = gate(0, 0);
        T('gate-normal-allows-swipe', normalGate.swipe && normalGate.useSurrounding);
        const pwGate = gate(8, 0);
        T('gate-password-taponly', pwGate.tapOnly && !pwGate.swipe && !pwGate.useSurrounding);
        const hintGate = gate(0, 128);
        T('gate-sensitive-hint-taponly', hintGate.tapOnly && !hintGate.swipe);

        const hello = GLib.getenv('SHEPHERD_HELLO');
        if (hello) {
            try {
                const [cands] = this._proxy.DecodeSync(hello, '');
                T('daemon-decode-hello', cands.length > 0 && cands[0][0] === 'hello',
                    `n=${cands.length} top=${cands[0]?.[0]} profile=${this._proxy.Profile}`);
            } catch (e) {
                T('daemon-decode-hello', false, `${e}`);
            }
        }
        console.log('SHEPHERD-SELFTEST DONE');
    }
}

// Shell-independent pieces of the GNOME backend: the D-Bus proxy to the decode daemon, the
// safety gate, and the qwerty-en-v1 geometry. Kept free of `gi://St`/Shell imports so it can
// be reasoned about (and, in principle, unit-tested under plain gjs) on its own.

import Gio from 'gi://Gio';

const BUS_NAME = 'com.armeafamily.ShepherdSwipe';
const BUS_PATH = '/com/armeafamily/ShepherdSwipe';

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

const DecoderProxyWrapper = Gio.DBusProxy.makeProxyWrapper(DECODER_IFACE);

/** Create a D-Bus proxy to the decode daemon on the session bus. */
export function makeDecoderProxy() {
    return DecoderProxyWrapper(Gio.DBus.session, BUS_NAME, BUS_PATH);
}

// Clutter.InputContentPurpose / InputContentHintFlags values (confirmed on GNOME Shell 50:
// PASSWORD=8, SENSITIVE_DATA=128, and there is NO PIN purpose — PIN fields arrive as
// PASSWORD or as DIGITS + the sensitive hint).
const PURPOSE_PASSWORD = 8;
const HINT_SENSITIVE_DATA = 128;

/**
 * Safety gate (host spec §4.4), mirroring shepherd-keyboard-core::safety. In a sensitive
 * field: no swipe, no suggestions, and the surrounding text must NOT be read.
 *
 * @param {number} purpose Clutter input content purpose (Main.inputMethod._purpose).
 * @param {number} hints   Clutter input content hint flags (Main.inputMethod._hints).
 */
export function gate(purpose, hints) {
    const sensitive =
        purpose === PURPOSE_PASSWORD || ((hints | 0) & HINT_SENSITIVE_DATA) !== 0;
    return {
        tapOnly: sensitive,
        swipe: !sensitive,
        suggestions: !sensitive,
        useSurrounding: !sensitive,
    };
}

// qwerty-en-v1 key geometry: normalized centers in [0,1], origin top-left. Verbatim from the
// bundle's layout.toml (and the wlroots renderer's fallback) so captured swipe coordinates
// align with the decoder's geometry. The proper long-term source is a daemon `Layout` method;
// embedding the fixed v1 geometry is fine until the layout set grows.
export const LAYOUT = {
    keyWidth: 0.1,
    keyHeight: 0.3333,
    keys: [
        {l: 'a', x: 0.10047, y: 0.5}, {l: 'b', x: 0.60047, y: 0.83333},
        {l: 'c', x: 0.40047, y: 0.83333}, {l: 'd', x: 0.30047, y: 0.5},
        {l: 'e', x: 0.25, y: 0.16667}, {l: 'f', x: 0.40047, y: 0.5},
        {l: 'g', x: 0.50047, y: 0.5}, {l: 'h', x: 0.60047, y: 0.5},
        {l: 'i', x: 0.75, y: 0.16667}, {l: 'j', x: 0.70047, y: 0.5},
        {l: 'k', x: 0.80047, y: 0.5}, {l: 'l', x: 0.90047, y: 0.5},
        {l: 'm', x: 0.80047, y: 0.83333}, {l: 'n', x: 0.70047, y: 0.83333},
        {l: 'o', x: 0.85, y: 0.16667}, {l: 'p', x: 0.95, y: 0.16667},
        {l: 'q', x: 0.05, y: 0.16667}, {l: 'r', x: 0.35, y: 0.16667},
        {l: 's', x: 0.20047, y: 0.5}, {l: 't', x: 0.45, y: 0.16667},
        {l: 'u', x: 0.65, y: 0.16667}, {l: 'v', x: 0.50047, y: 0.83333},
        {l: 'w', x: 0.15, y: 0.16667}, {l: 'x', x: 0.30047, y: 0.83333},
        {l: 'y', x: 0.55, y: 0.16667}, {l: 'z', x: 0.20047, y: 0.83333},
        {l: "'", x: 0.97, y: 0.5}, {l: '-', x: 0.86, y: 0.83333},
    ],
};

/** Nearest key label to a normalized point, or null. */
export function nearestKey(x, y) {
    let best = null;
    let bestD = Infinity;
    for (const k of LAYOUT.keys) {
        const d = (k.x - x) ** 2 + (k.y - y) ** 2;
        if (d < bestD) {
            bestD = d;
            best = k.l;
        }
    }
    return best;
}

/** Total arc length of a normalized point path (for tap-vs-swipe classification). */
export function arcLength(points) {
    let total = 0;
    for (let i = 1; i < points.length; i++)
        total += Math.hypot(points[i].x - points[i - 1].x, points[i].y - points[i - 1].y);
    return total;
}

/** Build a v1 gesture.json string (matches shepherd-swipe.gesture/1). */
export function toGestureJson(points) {
    return JSON.stringify({
        schema: 'shepherd-swipe.gesture/1',
        layout_id: 'qwerty-en-v1',
        points: points.map(p => ({x: p.x, y: p.y, t: p.t})),
    });
}

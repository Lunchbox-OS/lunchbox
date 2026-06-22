# shepherd-keyboard-gnome-daemon

The **GNOME backend's decode daemon**: a headless D-Bus service over
[`shepherd-keyboard-core`](../shepherd-keyboard-core).

GNOME exposes neither `input-method-v2`, `virtual-keyboard`, nor `layer-shell`, so the GNOME
backend is split in two: a GJS Shell extension (renders the keyboard, captures touch, and
commits text through GNOME's own input-method object) plus this Rust daemon, which owns the
decoder. The extension sends a captured gesture over D-Bus and gets ranked candidates back —
reusing the **identical** core decode path as the wlroots backend, so candidates match across
backends.

## Interface

Bus name `com.armeafamily.ShepherdSwipe`, object `/com/armeafamily/ShepherdSwipe`, interface
`com.armeafamily.ShepherdSwipe1`:

- `Decode(s gesture_json, s preceding_text) → a(sd)` — ranked `(word, score)`, best first.
  Empty on a malformed gesture or when no decoder is loaded. The caller **must** pass `""`
  for `preceding_text` in a sensitive field.
- `Available` (property, `b`) — whether predictions are available (false ⇒ tap-only).
- `Profile` (property, `s`) — the loaded profile id.

The daemon carries no editing/commit logic; that lives in the GJS extension (which commits via
GNOME's input-method object). Safety gating (password ⇒ tap-only, no surrounding text) is the
extension's responsibility on this backend — it must not call `Decode`, and must pass `""` for
preceding text, in sensitive fields.

## Running

```sh
scripts/fetch-swipe-bundles.sh
SHEPHERD_SWIPE_BUNDLE_DIR=dev-runtime/swipe-bundles \
  cargo run -p shepherd-keyboard-gnome-daemon -- --profile adult
```

Same flags as the wlroots backend (`--profile`, `--bundle-dir`, `--public-key`). Fails closed
(serves with no predictions) if no bundle verifies.

Verify it live on a private bus:

```sh
dbus-run-session -- bash -c '
  SHEPHERD_SWIPE_BUNDLE_DIR=dev-runtime/swipe-bundles \
    target/debug/shepherd-keyboard-gnome-daemon &
  sleep 1
  gdbus call --session --dest com.armeafamily.ShepherdSwipe \
    --object-path /com/armeafamily/ShepherdSwipe \
    --method com.armeafamily.ShepherdSwipe1.Decode \
    "$(cat crates/shepherd-keyboard-core/tests/fixtures/hello.gesture.json)" ""'
# => ([('hello', -9.51...), ('help', ...), ...],)
```

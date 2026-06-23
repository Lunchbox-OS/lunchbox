# shepherd-keyboard-wlroots

The **wlroots backend** for the swipe keyboard: a standalone Wayland client that renders the
keyboard and drives text entry through standard Wayland protocols. It is a thin adapter over
[`shepherd-keyboard-core`](../shepherd-keyboard-core) — all decode, candidate/commit policy,
editing-key semantics, and safety gating live there, so this crate only does Wayland I/O and
rendering.

Because it speaks only standard protocols (no `/dev/uinput`, no shepherd-launcher compositor
internals), it runs on **any** wlroots compositor and stays extractable.

## Protocols

- `zwp_input_method_v2` (via smithay-client-toolkit) — receives `activate`/`deactivate`,
  `content_type` (purpose + hint), and `surrounding_text`; emits `commit_string`,
  `set_preedit_string`, and `commit`. This is the text channel.
- `zwlr_layer_shell_v1` — a bottom-anchored surface with an exclusive zone so apps reflow.
- `zwp_virtual_keyboard_v1` — emits real keysyms (Enter / Backspace / Tab) that text commit
  can't express. A US xkb keymap is uploaded on startup.
- `wl_touch` (with a `wl_pointer` drag fallback) — captures swipes/taps, normalized against the
  rendered key area and fed to the core's `GestureBuilder`.

Rendering is software (SHM `ARGB8888`); key labels use a system TTF located at runtime
(skipped if none is found).

## Running

```sh
scripts/fetch-swipe-bundles.sh            # once, to cache the signed bundles
SHEPHERD_SWIPE_BUNDLE_DIR=dev-runtime/swipe-bundles \
  cargo run -p shepherd-keyboard-wlroots -- --profile adult
```

Flags: `--profile adult|child`, `--bundle-dir <root>` (or `$SHEPHERD_SWIPE_BUNDLE_DIR`),
`--public-key <file>` (production minisign trust anchor; the decoder's committed dev key is
used when unset), `--height <px>`. If the bundle can't be verified the keyboard **fails closed**
to tap-only with no predictions.

A headless connectivity smoke test (starts `sway --headless`, runs the client, asserts it binds
`input_method_manager_v2` and renders without crashing) lives at
`scripts/smoke-keyboard-wlroots.sh`.

## Try it out (out-of-band dev harness)

`scripts/dev-keyboard-wlroots.sh` opens a **nested Sway window** running the swipe keyboard plus a
text field, so you can swipe-type with the mouse (the pointer fallback stands in for touch) and
watch words commit — with no gdm and no shepherd-launcher:

```sh
scripts/fetch-swipe-bundles.sh
cargo build -p shepherd-keyboard-wlroots
scripts/dev-keyboard-wlroots.sh                       # --profile child, --app zenity, etc.
# In the window: swipe/tap to type; commits land in the text app. Quit with Alt+q.
```

The keyboard commits via `input-method-v2`, so the target must be a GTK/`text-input-v3` app
(`gnome-text-editor`, `zenity`) — a terminal only shows that the keyboard renders. `SWAY_HEADLESS=1`
runs the same bring-up without a window (for a CI/wiring check; combine with a `timeout`).

## Status

Implemented and clippy-clean; the connectivity smoke test passes against headless `sway`. The
remaining Phase 2 gate item — a fully automated headless test asserting a synthesized swipe
commits the correct word into a focused `text-input-v3` client, and that a password field is
tap-only — needs a text-input test client plus synthetic touch injection and is not yet wired.

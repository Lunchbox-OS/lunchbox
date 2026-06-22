# shepherd-keyboard-core

Backend-agnostic host logic for the swipe keyboard. This crate renders nothing and
talks to no compositor; it is the shared brain that both the wlroots backend and the
GNOME decoder daemon are thin adapters over, so identical gestures yield identical
candidates everywhere.

It consumes the external, host-agnostic decoder
[`shepherd-swipe-core`](https://git.armeafamily.com/albert/shepherd-swipe) (pinned to a
signed release tag) and the **signed bundles** it ships. The decoder verifies each
bundle's minisign signature and per-file hashes on load; this crate's job is to point it
at the right bundle, anchor trust in the correct public key, and **fail closed** (tap-only,
no predictions) if a bundle is missing, unverified, or incompatible.

Responsibilities (see `docs/ai/history/2026-06-21 001 swipe-ime-host-spec.md`):

- **Profile resolution** — which signed bundle (`adult` / `child`) to load, from
  shepherd-launcher policy; non-user-flippable in a child session.
- **Bundle loading** via `shepherd_swipe_core::Decoder`, fail-closed.
- **Layout model** loaded from the bundle's `layout.toml` (single source of truth for
  both rendering key positions and normalizing captured gestures).
- **Gesture assembly** — raw (down → motion\* → up) touch points in surface pixels →
  a normalized `*.gesture.json` gesture in the layout's coordinate space.
- **Decode + candidate policy** — call `decode`, choose commit behavior, manage the
  suggestion list, expose decode confidence so weak/OOV results fall back to letters.
- **Editing-key semantics** — space, backspace, enter, shift, symbols.
- **Safety-gate state machine** — input purpose → mode (password/PIN/sensitive ⇒ plain
  tap only, no suggestions, no surrounding-text reads).

## Portability / extractability

This crate depends only on `shepherd-swipe-core` plus small, portable utility crates —
**no Wayland, X11, GTK, or shepherd-launcher compositor internals.** That keeps the whole
keyboard extractable to its own repository later, and lets the wlroots backend run on any
wlroots compositor.

## Bundles for development and tests

Bundles are signed release artifacts, never committed. Fetch and verify them with:

```sh
scripts/fetch-swipe-bundles.sh
```

which downloads the pinned `v0.1.0` `adult`/`child` bundles into `dev-runtime/swipe-bundles/`
(gitignored) after checking `SHA256SUMS`. Tests that exercise the real decode path read the
bundle directory from `$SHEPHERD_SWIPE_BUNDLE_DIR` (default: `dev-runtime/swipe-bundles`) and
are `#[ignore]`d when it is absent — run them with `--include-ignored` once bundles are present.

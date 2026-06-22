# Swipe IME host — implementation notes (Phases 0–1)

Companion to the spec at `2026-06-21 001 swipe-ime-host-spec.md`. Records the recon
findings, the decisions taken, what was built, and the plan for the remaining phases, so a
later agent (or a human) can pick up cleanly.

**Prompt:** "Implement the just-added swipe keyboard spec."

## Decisions (owner-confirmed)

1. **Scope:** implement as far as is achievable in one session, reporting at each phase gate.
2. **Bundle delivery:** download the pinned signed release artifact and cache it
   (gitignored); nothing binary committed. → `scripts/fetch-swipe-bundles.sh`.
3. **Profile identity:** model it in the core now (`Profile` + per-profile bundle dir) and
   wire it to a `[service.keyboard]` config field / richer session policy in a later phase.
   shepherd-launcher has **no** adult/child session identity today.
4. **wlroots toolkit:** `smithay-client-toolkit` raw Wayland (NOT GTK4) — GTK4 cannot act as
   an `input-method-v2` client, and raw Wayland keeps the keyboard extractable.

## Recon findings (verified, not from memory)

### Decoder contract (`shepherd-swipe`, tag `v0.1.0`)

- Real clone URL is `https://git.armeafamily.com/albert/shepherd-swipe` (the spec's and
  `integration.md`'s `forge.albertarmea.com/...` URL is a placeholder). Tag `v0.1.0` is an
  annotated tag resolving to commit `4cf4e46`; cargo's `tag = "v0.1.0"` resolves it fine.
- **Bundles are downloadable signed release artifacts** — the central risk in the spec is
  retired. The `v0.1.0` release ships `shepherd-swipe-{adult,child}-v0.1.0.tar.gz`,
  `SHA256SUMS`, and `minisign-dev.pub`. No Python training pipeline is needed to consume.
- Real API (from source): `Decoder::load(&Path)` (committed dev key) **and**
  `Decoder::load_with_key(&Path, &str)` (the production trust-anchor hook). `decode(&Gesture,
  &Layout, Context) -> Vec<Candidate>`; `Candidate { word, score }` (score = fused log-score,
  higher is better). `Gesture::from_json -> (Gesture, GestureRecord)`. `Layout { layout_id,
  key_width, key_height, proximity_sigma, keys: Vec<Key{label,x,y}> }`, `nearest_key`,
  `proximity`. `Decoder` is **not** `Debug`.
- Coordinates: normalized key-area space, origin top-left, `x,y ∈ [0,1]`, overshoot allowed.
  Gesture `t` is ms from gesture start. `layout_id` must match (`qwerty-en-v1`).
- The bundle `layout.toml` contains only the **28 character keys** (a–z, `'`, `-`). Space,
  backspace, enter, shift, and symbols are **host-owned** editing keys.
- Public key is `minisign-dev.pub` (embedded as `DEV_PUBLIC_KEY`), not `minisign.pub`;
  `keys/` lives in the decoder repo, not shepherd-launcher.
- Proven end-to-end: the `hello.gesture.json` fixture → `Decoder::load(adult)` → `decode`
  returns `hello` (top-1).

### shepherd-launcher conventions

- Workspace: edition 2024, resolver 2, GPLv3, no workspace MSRV/lints; crates use
  `{ workspace = true }`. CI: `cargo test --all-targets`, `cargo clippy --all-targets -- -D
  warnings`, `cargo fmt --all -- --check`, shellcheck. System deps already include
  `libwayland-dev`, `libxkbcommon-dev`, `libgtk4-layer-shell-dev`.
- **All rendering today is GTK4** (`shepherd-hud`, `shepherd-launcher-ui`); layer-shell via
  `gtk4-layer-shell`. **All synthetic input today is `/dev/uinput`** (`shepherd-bridge`).
  Nothing uses `zwp_input_method_v2`.
- Three spec assumptions that don't hold: the spec forbids `/dev/uinput` (so `shepherd-bridge`
  can't be reused); GTK4 can't be an IM-v2 client (so the backend must be raw Wayland); and
  there's no adult/child session identity yet.

## What was built (Phases 0–1, green)

New crate **`crates/shepherd-keyboard-core`** — backend-agnostic, portable (depends only on
`shepherd-swipe-core` + serde/thiserror/tracing; no Wayland/GTK), so the keyboard stays
extractable.

- `error.rs` — fail-closed `Error` (`BundleMissing`, `Decoder`).
- `profile.rs` — `Profile { Adult, Child }` (id ↔ parse).
- `bundle.rs` — `bundle_dir(root, profile)` and `load_decoder(dir, public_key: Option<&str>)`;
  fail-closed on missing dir / verification failure; `load_with_key` is the production
  trust-anchor path.
- `safety.rs` — `InputPurpose` (mirrors the wlroots/GNOME purpose enum), `ContentType`,
  `SafetyGate`, `SafetyMode {Full, TapOnly}`. Password/PIN/sensitive ⇒ TapOnly (no swipe, no
  suggestions, no surrounding-text use).
- `gesture.rs` — `KeyArea` (pixel→normalized), `RawPoint`, `GestureBuilder` (time-rebased,
  non-decreasing), `Stroke {Tap, Swipe}` with arc-length tap/swipe classification.
- `policy.rs` — `CandidatePolicy` (max_suggestions, min_confidence) → `Decision`
  (preedit/suggestions/top_score/confident). Default previews top-1; a tuned threshold makes
  weak/OOV fall back to letters.
- `session.rs` — `Keyboard` state machine emitting `HostAction`s
  (`SetPreedit`/`CommitText`/`KeyInput`/`SetSuggestions`). Editing-key semantics (space
  finalizes preedit; backspace edits preedit else emits a Backspace keysym; enter finalizes +
  Enter keysym; shift off→one-shot→locked; symbols toggle), suggestion selection, and all
  safety gating. `tap_only(layout)` degraded mode + `fallback_layout()` (rendering-only
  vendored QWERTY geometry for when no bundle verifies).

Wiring: workspace member + `shepherd-swipe-core` pinned git dep in the root `Cargo.toml`;
`scripts/fetch-swipe-bundles.sh` (downloads, checks `SHA256SUMS`, extracts to
`dev-runtime/swipe-bundles/{adult,child}`).

Tests: 27 unit tests (default, no bundle) + 6 `#[ignore]`d e2e tests run via
`SHEPHERD_SWIPE_BUNDLE_DIR=… cargo test -p shepherd-keyboard-core -- --include-ignored`
(fixture decode, swipe→preedit→space-finalize, next-swipe finalize, suggestion select,
password field disables swipe/decode, password focus drops preedit without committing). The
unsigned-bundle fail-closed and password-gate tests satisfy the Phase 1 gate.

**Gates met:** Phase 0 (workspace builds with the dep; fixture decodes end-to-end) and Phase 1
(core unit tests pass incl. password-gate + unsigned-bundle fail-closed). `cargo build
--workspace`, fmt, and clippy are clean.

## Phase 2 — wlroots backend (`crates/shepherd-keyboard-wlroots`, done; one gate item residual)

A standalone `smithay-client-toolkit` Wayland client (binary), thin adapter over the core.

- SCTK turned out to wrap `input-method-v2` natively (`InputMethod` + `InputMethodHandler` +
  `delegate_input_method!`) and provides `touch`/`layer-shell`/`shm`; only
  `zwp_virtual_keyboard_v1` is bound manually (no events). This is why the backend is far
  smaller than the spec implied.
- `input-method-v2` text I/O (activate/deactivate, `content_type`, surrounding text →
  `commit_string`/`set_preedit_string`/`commit`); `virtual-keyboard` for Enter/Backspace/Tab
  with an uploaded US xkb keymap; bottom-anchored `wlr-layer-shell` surface with exclusive
  zone; software SHM render of letter grid + suggestion bar + function row (fontdue labels
  from a system TTF); `wl_touch` capture (pointer fallback) → core `GestureBuilder`.
  Content purpose/hint map to the core's `ContentType`; the `InputMethodHandler` is `&self`
  so the latest IM state is recorded into a `RefCell` and drained by the main loop.
- Fails closed to tap-only with the vendored fallback layout when no bundle verifies.
- **Gate status:** compiles clippy-clean; `scripts/smoke-keyboard-wlroots.sh` (headless
  `sway`) confirms it loads the bundle, binds `input_method_manager_v2`, and renders without
  crashing. **Residual:** an automated headless test asserting a *synthesized swipe commits
  the right word into a focused `text-input-v3` client* (and password ⇒ tap-only) is not yet
  wired — it needs a text-input test client + synthetic touch injection (headless sway has no
  input devices). The commit/gating logic itself is covered by the core's e2e tests.

## Phase 3 — GNOME decode daemon (`crates/shepherd-keyboard-gnome-daemon`, done)

A headless zbus D-Bus service over the core. Bus `com.armeafamily.ShepherdSwipe`, interface
`com.armeafamily.ShepherdSwipe1`: `Decode(s,s)→a(sd)`, `Available` (b), `Profile` (s). The
GJS extension (Phase 4) will commit via GNOME's IM object and call this only for candidates.

- **Gate met:** the daemon's decode matches the core path on the shared `hello` fixture
  (`#[ignore]`d parity test) and a live `dbus-run-session` + `gdbus` call returns
  `[('hello', -9.52), ('help', …)]` — identical to wlroots. Fail-closed (no decoder ⇒ empty)
  unit-tested. clippy-clean.

## Remaining phases (not yet started)

- **Phase 2 residual:** the automated headless swipe-commit/password integration test above.
- **Phase 4 — GNOME GJS Shell extension** (suppress built-in OSK, render, commit via the IM
  object, call the daemon, correct `session-modes`; reliable input-purpose gating is a release
  blocker). Needs a real GNOME Shell; not unit-testable here.
- **Phase 5 — Profile/safety/packaging:** add `[service.keyboard]` to `shepherd-config`
  (profile, bundle root, trust-anchor key path), wire profile to launcher policy, fetch/stage
  bundles at install, choose the production trust anchor location (root-owned / verified boot).
- **Phase 6 — Docs, extractability check, CI** for the new crates (incl. running the
  bundle-gated decode tests in CI after `scripts/fetch-swipe-bundles.sh`).

## Open questions still to resolve (spec §9)

- Target GNOME Shell version(s) and whether a `gdm` login keyboard is needed.
- Confirm GNOME reliably exposes input purpose + surrounding text + commit (gates the
  password-safety guarantee).
- Production bundle trust-anchor location and how it's protected from the child.
- Bundle update/rollback policy; whether physical-keyboard `grab` is needed in v1.

# Spec: Swipe IME Host Backends — wlroots/input-method-v2 + GNOME

**Audience:** Claude Code (agentic implementation), working inside the `shepherd-launcher` repo.
**Goal:** implement the host/IME side of the swipe keyboard — the part that renders the keyboard, captures gestures, decodes them via `shepherd-swipe`, and commits text — for **two backends**: a **wlroots `input-method-v2`** backend (primary; matches shepherd-launcher's Sway environment) and a **GNOME** backend (Shell extension + Rust decoder daemon). **No `/dev/uinput`** in this iteration.

This consumes the already-built decoder at `https://git.armeafamily.com/albert/shepherd-swipe` (**tag `v0.1.0`**, MIT). This spec does **not** touch the model or training pipeline.

Numbers are starting points; **acceptance is defined by the per-phase gates and the safety invariants, not by matching them.**

---

## 0. Before you start (mandatory)

Report findings before writing code:

1. **Read shepherd-launcher conventions.** `AGENTS.md`, `CLAUDE.md`, `CONTRIBUTING`, and the existing `crates/` — especially how the project already creates **layer-shell surfaces** and renders them (reuse that toolkit/approach), how it manages session/launch, and the `shepherd-media-core` Android-portability discipline. House conventions win over anything here.
2. **Read the decoder's contract.** In `shepherd-swipe` at tag `v0.1.0`, read `docs/integration.md`, `docs/spec.md`, and `docs/gesture-format.md`. Take the **exact** public API (`Decoder::load` / `decode` signatures), the **gesture interchange schema** (`*.gesture.json`), the **bundle layout** (`encoder.onnx`, `lexicon.fst`, `contextlm.bin`, `layout.toml`, `metadata.toml`, `bundle.sig`), and the **crate-version → schema_version** compatibility note from there — do not reproduce them from memory or from this spec.
3. **Pin the dependency.** Add `shepherd-swipe-core` as a git dependency pinned to `v0.1.0`. Decide with the owner: vendored bundle vs. downloaded signed release artifact, and the **trust anchor** for the bundle's public key (it must live somewhere a child cannot rewrite — root-owned config, ultimately verified boot; the signing public key already lives in the repo's `keys/`).
4. **Confirm targets.** Verify the Sway/wlroots environment for backend 1, and the **specific GNOME Shell version(s)** you must support for backend 2 (GJS APIs drift across releases — pin and verify).

Small commits; one PR per phase; `cargo fmt` + `cargo clippy` + existing CI before each PR.

---

## 1. Scope

**In scope**
- `shepherd-keyboard-core` — backend-agnostic host logic (decode orchestration, layout, gesture assembly, candidate policy, safety gates, profile resolution).
- **wlroots backend** — Wayland client using `input-method-v2` + `layer-shell` + `virtual-keyboard`.
- **GNOME backend** — a Rust **decoder daemon** (D-Bus) + a **GJS Shell extension**.
- Profile (adult/child) selection and safety gating; packaging/launch within shepherd-launcher.

**Out of scope**
- `/dev/uinput`, XDG portal RemoteDesktop, KDE/`text-input`-only paths.
- Any model/training/bundle-format change (consume `v0.1.0` as-is).
- Multi-language/multi-layout beyond what the pinned bundle ships (QWERTY-English).

---

## 2. The decoder contract (consumption)

The decoder is host-agnostic and verifies its own bundle. Per `shepherd-swipe`:

- `Decoder::load(bundle_path)` **verifies the signature and per-file hashes** before use and refuses on mismatch — so the host's job is to point it at the right bundle and to anchor trust in the public key, not to re-implement verification.
- `decode(gesture, layout, context)` returns ranked candidates. `gesture` is built from captured touch points in the **canonical keyboard coordinates** the `*.gesture.json` schema defines; `layout` is the **single source of truth from the bundle's `layout.toml`** (use it both to position rendered keys and to normalize gestures — this guarantees capture geometry matches decode geometry); `context` carries preceding text for ContextLM re-ranking.
- Pin a crate version **and** a bundle version together, per the compatibility matrix.

---

## 3. Architecture

### 3.1 `shepherd-keyboard-core` (backend-agnostic — build this first)
No windowing, no Wayland, no GJS. Owns:
- **Profile resolution** → which signed bundle to load (from session identity via shepherd-launcher's existing policy mechanism; non-bypassable).
- **Bundle loading** via `shepherd-swipe-core::Decoder`; **fail-closed** if missing/unverified (degrade to tap-only with no predictions, never load an unverified bundle).
- **Layout model** loaded from the bundle's `layout.toml`.
- **Gesture assembly**: raw (down → motion\* → up) touch points → normalized `*.gesture.json` gesture.
- **Decode + candidate policy**: call `decode`, choose commit behavior, manage the suggestion list.
- **Editing-key semantics**: space, backspace, enter, shift, symbols.
- **Safety-gate state machine**: input-purpose → mode (see §4.4).

Both backends are thin adapters over this; identical gestures must yield identical candidates across backends.

### 3.2 wlroots backend (primary)
A Wayland client:
- **Text I/O:** `zwp_input_method_manager_v2` → `zwp_input_method_v2`. Consume `activate`/`deactivate`, `surrounding_text`, `text_change_cause`, `content_type` (purpose + hint). Emit `commit_string`, `set_preedit_string`, `delete_surrounding_text`, and the matching `commit` with serial.
- **Surface:** `zwlr_layer_shell_v1`, bottom-anchored with an exclusive zone so apps reflow. Render with shepherd-launcher's existing surface/rendering stack.
- **Special keys:** `zwp_virtual_keyboard_v1` for keysym events that aren't text (Enter, Tab, arrows) where `commit_string`/`delete_surrounding_text` won't do.
- **Capture:** `wl_touch` for swipes/taps; pointer drag as a non-touch fallback.
- Works on any wlroots compositor, not only shepherd-launcher's Sway.

### 3.3 GNOME backend (Shell extension + daemon)
GNOME exposes neither `input-method-v2`, `virtual-keyboard`, nor `layer-shell`, so this backend is structurally different and **version-fragile**. Precedent: third-party OSKs on GNOME are Shell extensions forked from GNOME's default keyboard that rely on ibus and pop up on touch input events.

- **Rust decoder daemon** — headless wrapper over `shepherd-keyboard-core`, exposing `decode(gesture_json) → candidates` over **D-Bus** (session bus). No UI, no Wayland. Reuses the exact same core + bundle as the wlroots backend.
- **GJS Shell extension** — suppresses GNOME's built-in OSK; renders the keyboard actor inside the Shell; captures touch on its actor; reads focus, **input purpose/hints**, and surrounding text and commits text **through GNOME's own input-method object** (the same channel the built-in OSK uses — no IBus engine, no uinput); calls the daemon over D-Bus for candidates. Declare `session-modes` deliberately: `user` (and `gdm` only if a login-screen keyboard is needed); `unlock-dialog` is restricted because GNOME's review guidelines disallow connecting to keyboard signals in that mode — default to excluding it.

---

## 4. Behavior (both backends must honor)

### 4.1 Show / hide
Appear on text-field focus and/or touch; hide on deactivate/blur. (GNOME OSKs conventionally trigger on touch events.)

### 4.2 Swipe path
Capture gesture → assemble `*.gesture.json` → `decode` → candidates. **Default commit policy:** commit top-1 as preedit, show alternates in a suggestion bar, finalize on the next action (space/next swipe/tap-away). Kid-friendly: easy one-tap correction to an alternate. Make decode confidence legible so weak/OOV results fall back to letter entry rather than forcing a wrong word.

### 4.3 Tap / editing
Letter taps, space, backspace (`delete_surrounding_text(1)` or virtual-keyboard backspace), Enter (real keysym via virtual-keyboard on wlroots; IM commit on GNOME), shift/symbols. Space finalizes any pending preedit.

### 4.4 Safety gates (non-negotiable)
- **Password/PIN/sensitive purpose → plain tap only:** disable swipe, disable suggestions, do **not** read or use surrounding text. wlroots reads this from `content_type` purpose; GNOME reads input purpose via the Shell input-method object. **Validate that GNOME exposes purpose reliably on the target version** — if it cannot be reliably detected, treat that as a release blocker, not a silent degrade.
- **Profile:** load the adult/child bundle from session identity; **no profile-switch UI in a child session**; profile is resolved by shepherd-launcher policy, not user-flippable.
- **Fail-closed:** missing/unverified/incompatible bundle → tap-only, no predictions; never load unverified.

The child-safety property itself (profanity unemittable) lives in the bundle and is already enforced by closed-vocabulary decoding; the host's responsibility is to load the correct signed bundle and to gate password fields.

---

## 5. Security & extractability

- **Trust anchor:** the public key the decoder verifies against must be unrewritable by the child (root-owned; ultimately verified boot). The host binary itself must be protected from a "skip the check" edit — signing only helps if neither the key nor the verifier can be swapped.
- **Extractable by design:** the IME's eventual home is undecided. Keep `shepherd-keyboard-core` and the backends depending only on `shepherd-swipe-core` plus minimal shared `shepherd-*` utility crates, and interact with the compositor **only via standard Wayland protocols** — no coupling to shepherd-launcher's compositor internals. This lets the whole keyboard move to its own repo later with no surgery, and lets the wlroots backend run on any wlroots compositor.

---

## 6. Testing

- **Core:** unit tests for gesture assembly, layout normalization, candidate/commit policy, the safety-gate state machine, profile resolution, and **fail-closed on a bad/unsigned bundle**.
- **wlroots:** headless wlroots/Sway integration — synthesize touch, assert the correct word is committed into a test `text-input` client; assert a password-purpose field yields tap-only with no suggestions and no surrounding-text reads.
- **GNOME:** daemon-level decode test (gesture → candidates) with parity against the wlroots path on shared fixtures; a documented **manual integration checklist** on the target GNOME version (OSK suppression, render, commit via IM, purpose gating) — full automation of a Shell extension is impractical, so make the manual steps explicit and reproducible.
- **Cross-backend parity:** identical fixture gestures → identical candidates on both backends.

---

## 7. Phased delivery (obey the gates — stop and report on failure)

- **Phase 0 — Recon & wiring.** Complete §0. Add the pinned `shepherd-swipe-core` dep; scaffold `shepherd-keyboard-core`; decode a repo fixture gesture (e.g. `hello.gesture.json`) through the core to prove the pipeline.
  *Gate:* shepherd-launcher builds with the dependency; a fixture gesture decodes end-to-end through `shepherd-keyboard-core`.

- **Phase 1 — `shepherd-keyboard-core`.** Layout-from-bundle, gesture assembly, decode + candidate policy, editing-key semantics, safety-gate state machine, profile resolution, fail-closed.
  *Gate:* core unit tests pass, including the password-gate and the unsigned-bundle fail-closed test.

- **Phase 2 — wlroots backend.** `input-method-v2` + `layer-shell` + `virtual-keyboard`; surface render; `wl_touch` capture; commit; purpose + surrounding-text.
  *Gate:* headless integration — a swipe commits the correct word into a test field; a password field is tap-only with no suggestions.

- **Phase 3 — GNOME decoder daemon.** D-Bus service over `shepherd-keyboard-core`.
  *Gate:* daemon decode matches the wlroots path on shared fixtures.

- **Phase 4 — GNOME Shell extension.** Suppress built-in OSK; render; capture touch; commit via the Shell input-method object; call the daemon; correct `session-modes`.
  *Gate:* manual integration checklist passes on the target GNOME version, including **reliable input-purpose gating** for password fields.

- **Phase 5 — Profile, safety, packaging.** Wire profile identity to shepherd-launcher policy; bundle trust anchor + fail-closed; launch/packaging within shepherd-launcher for both backends.
  *Gate:* a child session loads the child bundle with no profile-switch path; an unverified bundle fails closed to tap-only.

- **Phase 6 — Docs, extractability, CI.** Design notes under `docs/ai`; confirm keyboard crates depend only on the allowed set (extractability check); CI for the new crates.
  *Gate:* extractability check passes; CI green.

---

## 8. Out of scope / non-goals

- No `/dev/uinput`, no portal injection, no KDE backend in this iteration.
- No model, bundle-format, or training changes.
- No reliance on shepherd-launcher compositor internals (standard Wayland protocols only).

---

## 9. Open questions to resolve and report

1. Target **GNOME Shell version(s)** and whether a login-screen (`gdm`) keyboard is required.
2. On GNOME, confirm the Shell input-method object reliably exposes **input purpose + surrounding text + commit** on the target version (vs. needing an IBus engine). This gates the password-safety guarantee.
3. The **rendering toolkit** for the wlroots surface — reuse shepherd-launcher's existing layer-shell rendering; confirm which.
4. How **profile identity** is resolved in shepherd-launcher today (find and reuse the existing mechanism).
5. **Bundle public-key trust anchor** location and how it's protected from the child (coordinate with the verified-boot / root-owned-config story).
6. Whether physical-keyboard **grab** (`grab_keyboard` on `input-method-v2`) is needed in v1 or deferred.
7. Bundle delivery into shepherd-launcher: vendored vs. downloaded signed release artifact, and the update/rollback policy.

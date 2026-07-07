# shepherd-media-android: easier library-adding without a keyboard

Date: 2026-07-06
Branch: `u/albert/70/media-android-optional-library-fields` (off
`u/albert/70/shepherd-media-android`)

## Prompt

> for shepherd-media-android, suggest ways to make it easier to add libraries
> on a Google/Fire TV device, where there is no keyboard

## Problem

The **Add library** form (`crates/shepherd-media-android/src/ui.rs`,
`add_library_screen`) asks the user to type four things:

| Field | Value | Typing pain on a D-pad |
|---|---|---|
| `Id` | `[a-z0-9-]+`, 1..=64 | High — arbitrary string |
| `Label` | free text, 1..=128 | High |
| `Source` | combo box | None — D-pad friendly |
| `Location` | a full URL / `content://` / path | Worst — long, mixed-case |

Three of four fields are free text, and the locator is a long URL. On a TV
remote this is hunt-and-peck.

## Hardware finding (Pixel 10a, on-device via adb)

An earlier hypothesis — "there is no IME bridge, so the soft keyboard never
shows" — was **tested and found wrong**, and the real gap is more specific:

- The media app is a pure `NativeActivity` egui `cdylib` (eframe 0.34 / egui
  0.34 / winit 0.30 / android-activity 0.6). There is no explicit
  `InputMethodManager`/`set_ime_allowed` code in the crate.
- **Touch path works.** Tapping the `Location` field brings up Gboard
  (`dumpsys input_method` → `mInputShown=true`, an `InputMethod` window is
  present) and text can be entered. eframe/winit wire the IME on pointer focus.
- **D-pad path is the real gap.** Driving the form with only DPAD arrows + OK
  (the sole inputs a Fire/Google TV remote has): DPAD-Down moves the focus ring
  onto the `Location` field (so navigation reaches it), but pressing OK/center
  does **not** open the keyboard (`mInputShown=false`); the press is consumed as
  an activation and focus bounces back to the `Back` button. A remote-only user
  can highlight the locator field but has no way to type into it.

So text entry is effectively unreachable without a touchscreen or an attached
hardware keyboard. Screenshots captured during the session:
switcher → settings → add-library form → tap-opens-keyboard →
dpad-focus-on-field → ok-does-not-open-keyboard.

## Suggestions (ranked)

1. **Auto-derive `Id`/`Label`** (done here). Neither needs manual entry; core
   already derives ids/titles from filenames, m3u names, and YouTube `list=`
   ids. Make both optional and fill them from the source. Biggest win for the
   least code; helps every path.
2. **SAF system file picker** (already on the crate's "not yet wired" list).
   `ACTION_OPEN_DOCUMENT` turns "TOML file (device)"/"M3U" into a visual browse
   with no keyboard, reaching USB/network shares via SAF providers. The system
   picker has its own input, so it sidesteps the egui IME gap entirely.
3. **Phone hand-off for URLs.** App shows a QR + LAN address; the user opens it
   on a phone (real keyboard), submits the URL, TV polls and pre-fills. Add
   `_shepherd-media._tcp` mDNS for discovery. Mirrors the existing
   `shepherd-ble` + `companion-android` + `shepherd-pairing-display` pattern
   (precedent, not a live link — the media app has no host connection).
4. **Voice input.** Fire/Google TV remotes have a mic; wire the IME voice
   button or `SpeechRecognizer` to dictate a playlist name / short URL.
5. **Share/deep-link intent.** Register an intent filter for `.toml`/`.m3u`/
   playlist URLs so sharing such a link from a browser/QR-scanner pre-fills the
   form.
6. **Fix the D-pad → IME gap.** Make OK on a focused text field call
   `set_ime_allowed(true)` / show the soft input, so remote users *can* type
   when they must. Fork in the road with the no-type paths above: either fix
   input, or design so typing on the TV is never required.

## Implemented in this branch (suggestion #1)

`crates/shepherd-media-app/src/settings.rs`:

- `LibrarySource::suggested_id()` / `suggested_label()` — derive a valid id and
  a placeholder label from the locator (file stem for file/URL sources, `list=`
  id for YouTube), with per-kind fallbacks (`library`/`playlist`/
  `youtube-playlist`). Backed by a local `slugify_id` matching core's semantics.
- `AppSettings::unique_id(base)` — returns `base`, else `base-2`, `base-3`, …,
  trimming to stay within the 64-char id budget.
- Unit tests for slugging, per-kind derivation, always-valid ids, dedup, budget,
  and the blank-fields add path.

`crates/shepherd-media-android/src/ui.rs`:

- `Id`/`Label` are now optional (hint text "optional — from source"). On Add,
  blank fields are derived from the source; a derived id is de-duplicated, while
  an explicitly typed duplicate still errors so the user learns about it.

Not done (follow-ups): filling `Label` from the resolved library's real title
after the source is fetched (needs the async resolve → settings write-back);
suggestions #2–#6.

## Verification

- `cargo test -p shepherd-media-app` — 44 pass (8 new).
- `cargo clippy -p shepherd-media-app -p shepherd-media-android --all-targets
  -- -D warnings` — clean.
- `cargo fmt --all` applied.
- On-device (Pixel 10a, debug APK): the form shows "optional — from source" on
  both fields; entering only `/sdcard/Kids Movies.toml` with Id/Label blank adds
  a library shown as "Kids Movies (kids-movies)" — label humanized from the file
  stem, id slugified. Confirmed working.

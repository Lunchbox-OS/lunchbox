# shepherd-media-android: armeabi-v7a support for 32-bit Fire TV

## Prompt

> connect to the Fire TV at 192.168.0.39 and deploy shepherd-media-android to it

Then, after discovering the ABI mismatch below:

> add armeabi-v7a support; keep it in this branch

## Context

Deploying to the Fire TV at `192.168.0.39` (`adb connect …:5555`) surfaced a
device that is **32-bit only**: model `AFTHA004` ("hazel") reports
`ro.product.cpu.abilist = armeabi-v7a,armeabi` with no `arm64-v8a`. The app was
built `arm64-v8a`-only, so `adb install` failed with
`INSTALL_FAILED_NO_MATCHING_ABIS (res=-113)`.

## What was done

Added `armeabi-v7a` as a second packaged ABI so the same APK installs on both
32-bit sticks and 64-bit phones/TVs.

1. **Vendored the 32-bit native libs.** The current `arm64-v8a/` libs byte-match
   the `dev.jdtech.mpv:libmpv` **1.0.0** AAR (mpv v0.41.0). That same AAR ships
   `armeabi-v7a` (and `x86`/`x86_64`). Extracted the `armeabi-v7a` `jni/` libs
   into `vendor/libmpv/armeabi-v7a/`, mirroring the arm64 file set exactly
   (9 `.so`s; dropped the AAR's `libplayer.so`, which this app doesn't use).

2. **`build.rs`** — mapped `target_arch = "arm"` → `armeabi-v7a` for the link
   search path.

3. **`android/app/build.gradle.kts`** — `rustAbis = listOf("arm64-v8a", "armeabi-v7a")`
   (drives both the packaged `abiFilters` and the `cargo-ndk` cross-compile).

4. **`Cargo.toml`** — the real blocker. `libmpv2-sys` by default copies
   `pregenerated_bindings.rs`, whose `__bindgen_test_layout_*` asserts bake in a
   **64-bit** struct layout (e.g. `mpv_stream_cb_info` = 48 bytes for 6 pointers).
   On 32-bit armv7 that struct is 24 bytes, so the const assertions fail to
   compile (`attempt to compute 24 - 48, which would overflow`). Fix: force
   `libmpv2-sys` to regenerate with bindgen instead. It isn't a direct dep, so it
   was added as an **Android-only** direct dep with `features = ["use-bindgen"]`
   under `[target.'cfg(target_os = "android")'.dependencies]`; feature
   unification then turns bindgen on for the transitively-used copy, for Android
   builds only. The host build (desktop preview, CI) keeps the pregenerated path.

   bindgen needs `libclang` on the build host (present: `libclang-21`).
   `cargo-ndk` sets `BINDGEN_EXTRA_CLANG_ARGS_<triple>` (sysroot + `--target`) so
   bindgen picks the correct per-ABI data model automatically.

Also installed the `armv7-linux-androideabi` Rust target.

## Build / deploy

```sh
export ANDROID_HOME=/opt/android-sdk
export ANDROID_NDK_HOME=$ANDROID_HOME/ndk/27.2.12479018
export LIBCLANG_PATH=/usr/lib/x86_64-linux-gnu   # for bindgen
rustup target add armv7-linux-androideabi
cd crates/shepherd-media-android/android && ./gradlew assembleDebug
adb -s 192.168.0.39:5555 install -r app/build/outputs/apk/debug/app-debug.apk
```

## Verification

- APK packages both `lib/arm64-v8a/` and `lib/armeabi-v7a/`.
- Installed to the Fire TV (`AFTHA004`) → `Success`.
- Launched: `NativeActivity` reaches Resumed, cold launch ~1.1s, GL surface up,
  no `UnsatisfiedLinkError` / `dlopen` failure / FATAL in logcat — i.e. the
  32-bit libmpv + cdylib load and run.
- Host `cargo check -p shepherd-media-android` still builds (pregenerated path
  unaffected); `cargo fmt --all -- --check` clean.

## Notes / not done

- Playback wasn't exercised end-to-end on the 32-bit stick (launch + native-load
  verified). Worth a follow-up smoke test of actual video on the AFTHA004.
- Only the debug APK was built/installed here.

## Follow-up: two TV UX fixes (same session)

> pressing the back button on the library screen should close the app, and there
> appears to be no default selection if no libraries are present, making it
> impossible to add the first one

Both surfaced while testing on the AFTHA004 (D-pad only, no pointer).

1. **No focus on the empty library switcher.** The switcher is excluded from the
   generic per-frame focus bootstrap because it "focuses its first library"
   explicitly — but the empty branch (`libraries.is_empty()`) rendered the
   "➕ Add a library" button and focused nothing, so the remote's center button
   had no target and the first library could never be added. Fix: focus that
   button when nothing else is (`src/ui.rs`, switcher's empty branch).

2. **BACK on the top-level switcher did nothing; should exit.** The back handler
   returned `None` for `Screen::Switcher`. `ViewportCommand::Close` was tried
   first but doesn't reliably finish a `NativeActivity` (winit stops its loop;
   Android keeps the activity — observed flaky: sometimes the process was killed,
   sometimes nothing happened). Replaced with a deterministic `Activity.finish()`
   over JNI: new `src/exit.rs` module (mirrors `insets`/`storage`: holds the
   activity pointer set from `android_main`, attaches to the JVM, calls
   `finish()`). Verified 3/3: BACK on the switcher moves the top activity from
   our `NativeActivity` to the Fire TV launcher. Note `finish()` ends the app's
   UI (returns to home) but leaves the process cached — expected Android
   behavior, not a leak, and cleaner than `Close` hard-killing the process.

## Follow-up: text fields trap D-pad focus (same session)

> if the focus ends up in a text box, it is impossible to leave the text box —
> the arrows don't exit it, and the back button goes to the previous screen
> entirely

A focused egui `TextEdit` sets a focus lock filter with
`horizontal_arrows: true, vertical_arrows: true` (arrows move the text cursor,
not focus). On a remote — which has no other way to move focus — that traps the
add-library form's Id / Label / Location fields. The user's only escape was BACK,
which the screen-level handler treated as "navigate up a screen".

Fixes (`src/ui.rs`, add-library form is the only screen with text fields):

- **Free the vertical arrows.** After each `TextEdit`, override its lock filter
  (`tv_free_field_focus`) to `vertical_arrows: false` so Up/Down move focus
  between the stacked fields and off to the buttons; keep `horizontal_arrows:
  true` so Left/Right still move the cursor (useful in the desktop preview,
  harmless on a remote where text is entered via the phone hand-off / file
  browser, not typed). Overriding *after* the widget runs wins for that frame's
  end-of-frame focus move.
- **BACK leaves the field, not the screen.** Track whether a field held focus
  this frame (`text_field_focused`); if so, BACK calls `Memory::stop_text_input()`
  (clears focus) instead of navigating up. Focus falls back to the screen's first
  control on the next frame; a second BACK then leaves the screen as before.

Verified on the AFTHA004: from a focused Id field, Down moves to Label (no longer
trapped); BACK releases the field and stays on the Add-library screen.

## Follow-up: add-library form D-pad navigation (same session)

> yes, fix the field navigation too

The aside above became the ask. egui's spatial focus walks the vertically-aligned
left-column buttons (Back / Browse / Add) and skips the offset field column, so a
remote's Down never reaches the fields — you had to press Right. Fixed by driving
focus explicitly in `add_library_screen` (`src/ui.rs`):

- **Tab-order Up/Down.** Capture every control's `Response` (Back, Id, Label,
  Source, Location, the one Browse/Phone action button, Add) into an ordered
  list; on Up/Down, find the focused one and `request_focus()` the previous/next
  (wrapping), then `move_focus(None)` to cancel egui's spatial move. Down now
  steps Back → Id → Label → Source → Location → Browse → Add. Verified on device.

Testing that surfaced two more remote gaps in the **Source `ComboBox`** — egui
only closes a combo popup on a *pointer click* or *Escape* (see
`popup.rs`: `close_click = … pointer.any_click()`), and Android sends BACK as
`BrowserBack`, not Escape:

- **Enter didn't close the popup.** A D-pad Enter fires the `selectable_value`
  (the kind changes) but leaves the popup open. Detect the change (`prev_kind`)
  and `Popup::close_all`, then re-focus the combo so focus doesn't drop to the
  first widget.
- **BACK skipped past the open popup and left the screen.** Snapshot
  `Popup::is_any_open` *before* the screen renders (egui may close it on Escape
  mid-render), and in the BACK handler dismiss an open popup first — ahead of the
  text-field and screen-navigation cases — so BACK closes the dropdown instead of
  navigating. While the popup is open, the form's Up/Down stepping is skipped so
  egui drives the options.

All verified on the AFTHA004: Down reaches every field; the Source dropdown opens,
navigates, selects (closing the popup, focus kept on Source), and BACK dismisses
it without leaving the screen. Back-handler order is now popup → text field →
screen; regression-checked that field-BACK and plain-BACK still behave.

## Follow-up: same fixes in the library editor, via shared helpers (same session)

> I need these fixes in the library *editor* too, not just the add library page.
> Ideally these should be pulled out into their own helpers rather than being
> copy/pasted.

The editor is the Settings screen's per-library `caching_editors` (three
`ComboBox`es — Cache / Quality / Posters — plus a Limit drag value). On device it
had the same faults: Down jumped `active` → `Remove`, skipping the combos, and the
combos didn't close on a D-pad Enter. Pulled the add-form logic into shared
helpers (`src/ui.rs`) and applied them to both screens:

- `tv_combo(ui, id_salt, &mut current, &[(value, label)])` — a D-pad-friendly
  `ComboBox`: shows the options, and on a pick closes the popup (`Popup::close_all`)
  and re-focuses the combo. Replaces all four inline combos (source kind + the
  three caching combos); the `cache_mode_label`/`poster_label` helpers became dead
  and were removed.
- `tv_focus_step(ui, &[&Response])` — the explicit Up/Down tab-order stepping,
  extracted verbatim. The caller builds the ordered response list (unavoidably
  imperative in egui); the helper does the wrap-around focus move and the
  popup-open skip. `caching_editors` now returns its three combo responses so
  `settings_screen` can thread them, the `active` toggle, and the move/remove
  buttons into one order. The Limit drag value is intentionally left out — it owns
  arrow-key handling and is reachable via Left/Right from Posters.
- `tv_free_field_focus` was already a helper (add-form text fields; the editor has
  no text fields). BACK-dismisses-open-popup stays where it was — once, globally,
  in the update loop — so it already covered the editor.

Verified on the AFTHA004: in the editor, Down now steps Back → Add → active →
Cache → Quality → Posters → Remove (reaching every combo), and each combo opens,
navigates, and selects (popup closes, focus kept). Add-form regression re-checked
after the refactor.

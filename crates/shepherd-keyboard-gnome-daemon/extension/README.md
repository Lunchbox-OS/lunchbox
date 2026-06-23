# Shepherd Swipe Keyboard — GNOME Shell extension

The GNOME half of the swipe keyboard. GNOME exposes neither `input-method-v2`,
`virtual-keyboard`, nor `layer-shell`, so this is a GJS Shell extension paired with the Rust
[`shepherd-keyboard-gnome-daemon`](..): the extension renders the keyboard, captures touch,
reads input purpose + surrounding text, and commits text through **GNOME's own input-method
object**; it calls the daemon's `Decode` over D-Bus for candidates, so GNOME and wlroots
decode through the identical core path.

Targets **GNOME Shell 50** (`metadata.json` `shell-version: ["50"]`); re-pin if the host moves.
`session-modes: ["user", "gdm"]` — a login-screen keyboard is wanted, so `gdm` is included;
`unlock-dialog` stays excluded per GNOME review guidelines that disallow keyboard-signal
connections there.

## Implementation

- `decoder.js` — D-Bus proxy to the daemon, the safety `gate()` (mirrors
  `shepherd-keyboard-core::safety`), the `qwerty-en-v1` geometry (verbatim from the bundle, so
  captured swipe coordinates align with decode geometry), and the v1 `gesture.json` builder.
- `extension.js` — builds the keyboard actor (suggestion bar + letter grid + function row) as a
  bottom-docked `addChrome` surface (reserves space via struts); tap → `Main.inputMethod.commit`;
  swipe → normalized `gesture.json` → daemon `Decode` → suggestion bar → tap-to-commit;
  Enter/Backspace via `Main.inputMethod.handleVirtualKey`; reads purpose/hints from
  `Main.inputMethod` and applies the gate; suppresses the built-in OSK by overriding
  `Main.keyboard.open`.
- **Focus-driven show/hide:** the keyboard starts hidden and shows when a text field is focused,
  hiding on blur. GNOME 50 has no IM focus signal, so it tracks `Main.inputMethod.currentFocus`
  driven by the `cursor-location-changed` / `surrounding-text-set` IM signals and
  `global.display notify::focus-window`.

Confirmed GNOME 50 APIs (empirically, since `Eval`/unsafe-mode is off): `inputMethod.commit`,
`inputMethod.handleVirtualKey`, `inputMethod.getSurroundingText`, `inputMethod._purpose` /
`._hints` (purpose `PASSWORD=8`, hint `SENSITIVE_DATA=128`; **no PIN purpose** — PIN arrives as
PASSWORD or DIGITS+sensitive), `layoutManager.keyboardBox`, `keyboard.open`.

## Validated on GNOME Shell 50 (user + gdm)

`scripts/validate-keyboard-gnome.sh [user|gdm]` runs the daemon + a nested headless `gnome-shell`
on a private bus, enables the extension with its built-in self-test, and asserts every check
passes. Confirmed in **both** `user` and `gdm` modes: the extension enables cleanly
(`state=ENABLED`, no error), the actor renders (28 keys, on stage), the commit + virtual-key
APIs are present, the safety gate forces tap-only for password/sensitive fields, a gesture
decodes through the daemon to top candidate `hello` (parity with wlroots), and the keyboard
**starts hidden** with working show/hide mechanics.

**Still needs an interactive session** (input devices + a focused app — not coverable headless,
which has no app to focus): show-on-real-focus / hide-on-blur end to end, a touch-driven swipe
committing into an app field, and built-in-OSK-suppression behavior. The manual checklist below
covers these.

## Login-screen (gdm) keyboard

A login-screen keyboard is in scope (`session-modes` includes `gdm`), which adds requirements
beyond the user-session keyboard:

- **The extension must be installed system-wide and enabled for gdm**, not just for the user.
  Install under `/usr/share/gnome-shell/extensions/shepherd-swipe@armeafamily.com` and add the
  uuid to gdm's enabled list (e.g. a gdm dconf profile setting
  `org.gnome.shell enabled-extensions`), since `~/.local/share` isn't read in the gdm session.
- **A daemon instance must run on the gdm session bus.** The greeter runs as the `gdm` user with
  its own session bus, so a `shepherd-keyboard-gnome-daemon` must be started there (e.g. a
  systemd user unit in the gdm slice, or D-Bus activation). It loads a bundle readable by the
  `gdm` user from a root-owned location.
- **Profile at the login screen:** there is no logged-in user yet, so the greeter has no
  child/adult identity — run the gdm daemon with the **adult** profile (or a dedicated neutral
  one). The password field is still tap-only via the safety gate, so no password text is
  decoded or fed to the context LM regardless of profile.
- `gdm` mode imposes the same `unlock-dialog` review constraints; keep keyboard-signal handling
  out of any locked mode.

## Open questions before this can ship (spec §9)

- **Confirm GNOME 50 reliably exposes input purpose + content hint + surrounding text + commit**
  through the Shell input-method object without an IBus engine — in **both** `user` and `gdm`
  modes. Per the spec this gates the password-safety guarantee: if purpose can't be reliably
  detected, that is a **release blocker**, not a silent degrade. (Target version and the
  login-screen requirement are now settled: GNOME 50, gdm keyboard wanted.)

## Manual verification checklist (the Phase 4 gate)

On the target GNOME Shell version, with the daemon running (`shepherd-keyboard-gnome-daemon`):

- [ ] Extension enables with no errors in `journalctl --user -f /usr/bin/gnome-shell`.
- [ ] Built-in OSK is suppressed; only this keyboard appears.
- [ ] Keyboard appears on text-field focus / touch and hides on blur.
- [ ] A swipe commits the expected word into the focused field (matches the daemon's `Decode`
      output and the wlroots backend on the same gesture).
- [ ] Suggestion bar shows alternates; tapping one commits it.
- [ ] Space/backspace/enter/shift/symbols behave as on wlroots.
- [ ] A **password/PIN field is tap-only**: no swipe, no suggestions, and surrounding text is
      never read (verify via logs that `Decode` is not called and preceding text is `''`).
- [ ] In a child session there is no profile-switch UI.
- [ ] **Login screen (gdm):** the keyboard appears at the GNOME greeter, the password field is
      tap-only, and the keyboard commits the username into the user field.

## Install (development)

User session:

```sh
ln -s "$PWD/crates/shepherd-keyboard-gnome-daemon/extension" \
  ~/.local/share/gnome-shell/extensions/shepherd-swipe@armeafamily.com
# restart GNOME Shell (log out/in on Wayland), then:
gnome-extensions enable shepherd-swipe@armeafamily.com
```

Login screen (gdm) — system-wide, since `~/.local/share` isn't read in the gdm session:

```sh
sudo cp -r "$PWD/crates/shepherd-keyboard-gnome-daemon/extension" \
  /usr/share/gnome-shell/extensions/shepherd-swipe@armeafamily.com
# enable for gdm via its dconf profile (org.gnome.shell enabled-extensions),
# and arrange a daemon instance on the gdm session bus (see "Login-screen keyboard").
```

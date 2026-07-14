# shepherd-waydroid-helper

Privileged helper for the Android (Waydroid) activity kind. shepherdd runs
unprivileged; this binary performs the Waydroid operations that need root,
invoked via `pkexec` under a polkit rule.

## Subcommands

```
shepherd-waydroid-helper force-stop --package <android.package.name>
shepherd-waydroid-helper preboot
shepherd-waydroid-helper lock-down
shepherd-waydroid-helper pin --package <android.package.name>
shepherd-waydroid-helper unlock
shepherd-waydroid-helper boot-completed
shepherd-waydroid-helper maximize --package <android.package.name>
```

- `force-stop` → `waydroid shell am force-stop <pkg>`. Reclaims the cached
  Android process after shepherdd closes the app's Wayland window.
- `preboot` → `systemctl start waydroid-container`. Brings the root LXC
  container service up so shepherdd can then start the user-level session.
  Takes no arguments; the unit name is hardcoded.
- `lock-down` → `waydroid shell cmd statusbar send-disable-flag <flags>`.
  Hardens the running session against a child leaving the kiosk app: disables
  the notification shade / quick settings (which can reach Android Settings) and
  the nav-bar home/recents/search buttons. Takes no arguments; the flag set is
  hardcoded.
- `pin` → drives the DPC's `LaunchActivity` to launch `<pkg>` pinned in Android
  Lock Task Mode (`lock_mode = "locktask"`). Same package trust boundary as
  `force-stop`; the DPC component is hardcoded.
- `unlock` → broadcasts to the DPC's `ControlReceiver` to clear the Lock Task
  allowlist so a locked session can end. Takes no arguments.
- `boot-completed` → exits 0 iff `waydroid shell getprop sys.boot_completed`
  prints `1` (Android inside the running session has finished booting). Takes no
  arguments. shepherdd's readiness gate polls this to keep Android activities
  hidden from the launcher until a launch would land on a booted system rather
  than the boot animation. It is a *query* that inspects the command's output, so
  it does not `exec` (getprop always exits 0).
- `maximize --package <pkg>` → grows the foreground app's freeform window to fill
  the display (`am task resize`). Waydroid opens each multi-window app
  (`lock_mode = "statusbar"` / `"off"`) in a small default freeform window and
  does not resize the Android task to follow the host window, so shepherdd calls
  this after launch to make the app fill its window (in freeform the app fills in
  landscape rather than honoring a portrait lock). Multi-step (read the top task
  + display size, verify `<pkg>` is on top, then resize), so like
  `boot-completed` it does not `exec`.

## Trust boundary

- Invoked via pkexec under a polkit rule that grants the action
  `org.shepherd.waydroid.helper` (declared in
  [`dist/polkit/org.shepherd.waydroid.policy`](../../dist/polkit/org.shepherd.waydroid.policy))
  password-less to members of the unix group `shepherd-waydroid` (see
  [`dist/polkit/50-shepherd-waydroid.rules`](../../dist/polkit/50-shepherd-waydroid.rules)).
- The only caller-controlled input is the **package name**. It is re-validated
  here with `shepherd_util::is_valid_android_package` — the *same* rule config
  uses, so the check cannot drift — which forbids leading `-`, whitespace, `/`,
  and shell metacharacters. The validated value is passed as a single argv
  element with **no shell**, so it cannot inject options or commands.
- `preboot`, `unlock`, and `boot-completed` take no arguments, and every fixed
  command (systemd unit, DPC components, getprop name) is hardcoded, so the
  actions cannot be aimed at any other service.
- Dependencies are limited to `shepherd-util` (for the shared validator) plus
  std, keeping the audit surface small.

Every subcommand but `boot-completed`/`maximize` `exec()`s a fixed command, so
the helper's pid becomes `waydroid`/`systemctl` and shepherdd's `pkexec …
.status()` returns when it exits. Those two instead run fixed, shell-free
commands, inspect their output, and return their own exit code — `boot-completed`
because getprop always exits 0, `maximize` because it must read the top task id
and display size before issuing the resize (the only variable parts of that
second command are integers it parsed).

## Install

Built and installed by `shepherd install` (and a dev script analogous to
`scripts/integration-tests/setup-firewall-dev.sh`). It must live at
`/usr/libexec/shepherd-waydroid-helper` — that path is the default baked into
shepherdd (overridable for development via `SHEPHERD_WAYDROID_HELPER`) and is
the path gated by the polkit action.

Setup also requires the `shepherd-waydroid` group:

```sh
sudo groupadd --system shepherd-waydroid
sudo usermod -aG shepherd-waydroid <kiosk-user>   # re-login for it to take effect
```

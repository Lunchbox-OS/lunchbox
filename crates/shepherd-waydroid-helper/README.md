# shepherd-waydroid-helper

Privileged helper for the Android (Waydroid) activity kind. shepherdd runs
unprivileged; this binary performs the two Waydroid operations that need root,
invoked via `pkexec` under a polkit rule.

## Subcommands

```
shepherd-waydroid-helper force-stop --package <android.package.name>
shepherd-waydroid-helper preboot
shepherd-waydroid-helper lock-down
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
- `preboot` takes no arguments and the systemd unit (`waydroid-container`) is
  hardcoded, so the action cannot be aimed at any other service.
- Dependencies are limited to `shepherd-util` (for the shared validator) plus
  std, keeping the audit surface small.

Both subcommands `exec()` a fixed command, so the helper's pid becomes
`waydroid`/`systemctl` and shepherdd's `pkexec … .status()` returns when it
exits.

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

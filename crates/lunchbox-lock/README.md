# lunchbox-lock

The screen lock for administrator mode (issue #154).

## Why it exists

Administrator mode puts a general-purpose desktop in front of a caregiver, on a
device whose whole design assumes the person at the keyboard is a child. Walking
away is a *supported* way to use the mode — waiting out a Steam download on a
slow connection is the motivating case — so the mode cannot simply close itself.
This is what makes that safe: the work keeps running, and the screen cannot be
touched until an administrator unlocks it from the companion or web app.

## Why it is not GTK

Every other lunchbox surface is GTK4 (`lunchbox-hud`, `lunchbox-launcher-ui`,
`lunchbox-pairing-display`). This one is a raw `wayland-client` +
`smithay-client-toolkit` program drawing with cairo into a shared-memory buffer,
because it must be a real `ext-session-lock-v1` client rather than a
`gtk4-layer-shell` overlay.

The deciding property is what happens when the process dies. Under the lock
protocol the compositor keeps the session locked and paints a blank screen —
verified against sway 1.11, which paints it solid red. A layer surface is just a
window: anything that can reach the compositor can close it, and #148 recorded
that happening for real, an activity issuing `[app_id=com.lunchbox-os.hud] kill` to
remove the HUD for the rest of a session. The same command against a layer-shell
"lock" would unlock the device.

For a control whose entire job is keeping a determined child out, failing
*closed* is the requirement, and only the session-lock protocol offers it. The
cost is a second rendering stack, which is affordable because the content is two
lines of text.

## Lifecycle

lunchboxd spawns this process (`HostAdapter::set_locked(true)`) and unlocks it by
sending **SIGTERM**, which is handled: the lock is released through the protocol
and the process exits.

`SIGKILL` deliberately is not handled. It leaves the compositor holding an
abandoned lock with no client to release it — a screen that stays covered until
the session restarts. That is the safe direction, and it is why lunchboxd always
asks politely and waits for the exit rather than killing on a timeout.

A crashed lock **is** recoverable without a reboot: a fresh `lunchbox-lock` can
take the lock again and then release it, which is what pressing "unlock" does
after a crash. Verified end to end.

## How lunchboxd finds it, and why that has to be got right

`LinuxHost::set_locked` looks for the binary **beside its own executable**
(`current_exe()`'s directory), which is what makes a development build spawn the
lock client from `target/debug` rather than whatever is installed. Failing that
it falls back to the bare name, resolved through `lunchbox_host_linux::helpers`
against the compiled-in trusted directories rather than `$PATH` (issue #144):
this is the one binary standing between a child and a locked screen, so a name
an activity could satisfy by writing its own `lunchbox-lock` onto a
kiosk-chosen `PATH` is exactly what must not happen here.

**Both of those find nothing unless the binary is installed**, and on a device
the sibling probe looks in `/usr/bin`, where `lunchboxd` lives. So this crate
must be listed in `LUNCHBOX_BINARIES` in
[`scripts/lib/build.sh`](../../scripts/lib/build.sh) — the single list that
`binaries_exist`, `install_bins` and `uninstall_bins` all read, and the one the
`.deb` inherits, since packaging drives `install.sh` with `DESTDIR` set. It was
left out once, and the symptom was a device where `lock_device` answered

```
failed to start the screen lock at /usr/bin/lunchbox-lock: No such file or
directory (os error 2)
```

while every development session locked perfectly, because `target/debug` had it
sitting next to `lunchboxd` the whole time. Adding it to that list is also what
makes a build *fail* when it is missing, instead of shipping without it.

The error names the resolved path for the same reason: "not installed" and
"installed but unreadable" both answer `ENOENT`, and only the path says which
directory was actually looked in.

## Testing it by hand

Against the headless dev session (`./scripts/lunchbox dev headless`):

```sh
set -a; . dev-runtime/headless/session.env; set +a
./target/debug/lunchbox-lock &        # covers the screen
./scripts/lunchbox dev shot lock.png  # see it
pkill -TERM lunchbox-lock             # releases it
```

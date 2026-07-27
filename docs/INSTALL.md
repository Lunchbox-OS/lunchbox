# Installation

`shepherd-launcher` can be installed on Linux with a modern Wayland compositor.
It is currently developed and tested on Ubuntu 26.04.

`shepherd-launcher` can be installed from the apt repository (the quick path,
with automatic upgrades), from a standalone prebuilt `.deb`, or from source (for
development). `./scripts/shepherd` can help set up your build environment and
manage a source installation.

## Installing from the apt repository

Prebuilt amd64 packages are published to this project's Forgejo Debian package
registry, so you can install and then `apt upgrade` on future releases. Add the
repository's signing key and source list once (prereleases are deliberately not
published here, so `apt upgrade` only tracks stable versions):

```sh
sudo install -d -m 0755 /etc/apt/keyrings
sudo curl -fsSL https://git.armeafamily.com/api/packages/albert/debian/repository.key \
  -o /etc/apt/keyrings/forgejo-albert.asc
echo "deb [signed-by=/etc/apt/keyrings/forgejo-albert.asc] \
https://git.armeafamily.com/api/packages/albert/debian stable main" \
  | sudo tee /etc/apt/sources.list.d/shepherd.list
sudo apt update
sudo apt install shepherd-launcher
```

`apt` pulls in the runtime dependencies (Sway, mpv, BlueZ, …) from the Ubuntu
archive; the Forgejo repository only carries `shepherd-launcher` itself. Post-
install (package contents, per-user setup) is identical to the standalone `.deb`
below — continue with the `shepherd-admin setup-user` step described there.

## Installing from a standalone `.deb`

If you'd rather not add the apt repository, prebuilt amd64 packages are also
attached to each
[release](https://git.armeafamily.com/albert/shepherd-launcher/releases).
Download the `.deb` for the version you want and install it with `apt`, which
also pulls in the runtime dependencies (Sway, mpv, BlueZ, …):

```sh
sudo apt install ./shepherd-launcher_0.2.0_amd64.deb
```

The package installs the binaries, the privileged firewall helper and its
polkit assets, the `/dev/uinput` udev rule, the Sway kiosk session, and the
display-manager session entry. Its post-install step creates the
`shepherd-firewall` system group and reloads udev/polkit.

A distro package can't know which account is your kiosk user, so per-user setup
is **not** done automatically. The package ships a `shepherd-admin` CLI for the
post-install admin tasks (the same code the from-source `./scripts/shepherd`
runs). Deploy the example config and add the user to shepherd's groups with one
command (substitute your user for `kiosk`):

```sh
sudo shepherd-admin setup-user kiosk
```

If you use YouTube media libraries, also install `yt-dlp`. shepherd deliberately
does not use the apt `yt-dlp` — YouTube changes formats often and the archived
build goes stale — so it lives in a venv you can refresh independently:

```sh
sudo shepherd-admin yt-dlp install   # re-run periodically to update
```

To install an activity backend (Steam via Canonical's snap, Chrome via Flathub —
matching what shepherd's `type = "steam"` and `kind = "flatpak"` adapters drive):

```sh
sudo shepherd-admin apps install steam    # or: chrome
```

`apps install steam` also connects the snap's `mount-observe` interface and
permits unprivileged user namespaces (`kernel.apparmor_restrict_unprivileged_userns=0`
via `/etc/sysctl.d/90-shepherd-userns.conf`) — Steam's sandbox needs one, and
Ubuntu 23.10+ restricts them by default, otherwise Steam fails with "Steam now
requires user namespaces to be enabled." This relaxes that hardening
system-wide; remove the drop-in and reboot to revert.

To make the hardware power button sleep the device instead of shutting it down
(a long press still powers off):

```sh
sudo shepherd-admin power-key suspend
```

`shepherd-admin` also exposes `harden` (kiosk lockdown) and `bluetooth clear`
(reset a user to unclaimed); run `shepherd-admin --help` for the full list.

Then have `kiosk` log out and back in (so the new group memberships take
effect) and pick the "Shepherd Kiosk" session at login. Kiosk hardening is
still optional — see [below](#kiosk-hardening-optional).

> The companion `.apk` is attached to the same release; see
> [Installing the Android apps](#installing-the-android-apps) below.

## Installing the Android apps

Two Android apps ship alongside the launcher:

- **Shepherd Companion** — the parent-facing admin app. Pairs with a device over
  Bluetooth LE and drives the management RPCs. This is the one you want.
- **Shepherd Media** — the media player, for phones, tablets, and Fire TV sticks.

### From the F-Droid repository (recommended, gives updates)

Install the [F-Droid](https://f-droid.org) client, then add the "Armea Family
Apps" repository:

```
https://git.armeafamily.com/fdroid/repo?fingerprint=b3dc6194dca2d059c0714202b1ad11ec712581061e2be66b74a0b14abe15b422
```

The quickest way to get that onto a phone is to open
<https://git.armeafamily.com/fdroid/repo/> on the device and scan the QR code
there, which encodes the same URL. The `fingerprint` pins the repository's
signing key: a client that has it will reject an index signed by anything else,
which is what makes an unattended background update safe. It is published here
so you can check it against the page rather than taking the page's word for it.

On Android 12 and newer, F-Droid updates apps **it installed** in the background
with no prompting, so a phone with the companion app tracks new releases the way
`apt upgrade` tracks the `.deb`. Two caveats worth knowing:

- If you sideloaded the app previously, the signature matches, so F-Droid offers
  the update rather than making you uninstall — but the *first* update through
  F-Droid still prompts. After that F-Droid is the installer of record and later
  updates are silent.
- Fire TV sticks run Android 9–11, which has no unattended-update path, and the
  F-Droid client has no remote-friendly TV interface. On the sticks, `adb` below
  remains the practical route.

### By sideloading

Every release attaches both APKs. Download the one you want from the
[releases page](https://git.armeafamily.com/albert/shepherd-launcher/releases)
and install it:

```sh
adb install shepherd-companion_0.3.0.apk
```

Both APKs are signed with the same key across releases, so an `adb install` over
an existing install upgrades it in place.

## Basic setup

The following builds and installs a fully functional local kiosk from source.

```sh
# 0. Install build dependencies
sudo ./scripts/shepherd deps build run

# 1. Install runtime dependencies
sudo ./scripts/shepherd deps install run

# 2. Build release binaries
./scripts/shepherd build --release

# 3. Install everything for a kiosk user
sudo ./scripts/shepherd install all --user kiosk
```

This installs:
- Binaries to `/usr/local/bin/`
- System Sway configuration to `/etc/sway/shepherd.conf`
- Drop-in directory for Sway overrides at `/etc/sway/shepherd.conf.d/`
- Display manager desktop entry ("Shepherd Kiosk" session)
- User config to `~kiosk/.config/shepherd/config.toml`

`/etc/sway/shepherd.conf` is regenerated by `shepherd install sway-config`,
so anything you want to keep across upgrades belongs in
`/etc/sway/shepherd.conf.d/*.conf` instead. For example, to scale a HiDPI
display:

```sh
sudo tee /etc/sway/shepherd.conf.d/hidpi.conf <<'EOF'
output * scale 2
EOF
```

See `man 5 sway-output` for the full set of `output` directives.

### External monitor / docking (issue #87)

If you use external-monitor mirroring, the Sway session must be started with
`WLR_SCENE_DISABLE_DIRECT_SCANOUT=1` in its environment. Without it, a
fullscreen activity direct-scans-out and starves `wl-mirror`'s screen capture,
blacking the mirrored display. This is a compositor-startup environment variable
(read by wlroots, not settable from `sway.conf`), so set it wherever the kiosk
session is launched — e.g. in the "Shepherd Kiosk" desktop entry's `Exec`, or a
drop-in `environment.d`/PAM env file for the kiosk user:

```sh
echo 'WLR_SCENE_DISABLE_DIRECT_SCANOUT=1' | sudo tee -a /etc/environment
```

Mirroring also requires the `wl-mirror` package (installed by
`shepherd deps install run`).

For custom installation paths:
```sh
# Install to /usr instead of /usr/local
sudo ./scripts/shepherd install all --user kiosk --prefix /usr
```

Individual installation steps can be run separately:
```sh
sudo ./scripts/shepherd install bins --prefix /usr/local
sudo ./scripts/shepherd install config --user kiosk
```

### Uninstalling

`shepherd uninstall` reverses a from-source install, removing the files
`shepherd install` placed system-wide (binaries, firewall helper + polkit
assets, sway config, desktop entry, udev rule). It deliberately leaves
per-user config under `~/.config/shepherd` and group memberships in place —
remove those by hand if you want them gone.

```sh
# Remove just the binaries (use the same --prefix you installed with)
sudo ./scripts/shepherd uninstall bins --prefix /usr/local

# Remove everything installed system-wide
sudo ./scripts/shepherd uninstall all
```

(If you installed the `.deb`, use `sudo apt-get remove shepherd-launcher`
instead.)

## Input compatibility sidecars (optional)

Activities can opt into one or more input-compat sidecars via the
`input_compat` entry (see `config.example.toml`):

- `touch_to_mouse` — for games that ignore raw touch events.
- `tablet_to_touch` — the inverse: synthesize touch from an absolute pointer
  or tablet, for activities that only handle touch.
- `disable_touch` — grab and discard all touch input, disabling the
  touchscreen for activities that misbehave on touch but still work with a
  mouse or gamepad.
- `gamepad_productivity` / `gamepad_gpd` — remap a gamepad to mouse +
  keyboard for activities that ignore gamepad input.

Most sidecars read `/dev/input/event*` and synthesize their output through a
`/dev/uinput` virtual device. They require the user to be in the `input`
group, plus a udev rule granting that group access to `/dev/uinput`. Using
uinput is what lets the sidecars work on any Wayland compositor (GNOME/Mutter,
KWin, …), not just wlroots ones like Sway. (`disable_touch` is the exception —
it only grabs input and emits nothing, so it needs the `input` group but not
`/dev/uinput`.)

`shepherd install all` adds the group and installs the udev rule
automatically. To set them up on an existing install, run:

```sh
sudo ./scripts/shepherd install groups --user kiosk
sudo ./scripts/shepherd install udev
```

The group change takes effect on the user's next login; the udev rule applies
after the next `/dev/uinput` access (or a reboot).

## Input-device dependencies (optional)

Activities can also declare a hardware dependency via `requires_input` (see
`config.example.toml`) — for example a typing tutor that should only appear
once a physical keyboard is connected. Supported types are `mouse`, `touch`,
`keyboard`, and `gamepad`.

Unlike the sidecars above, this is enforced by `shepherdd` itself: it reads
`/dev/input/event*` to see which device types are attached and hides gated
activities until they are. It therefore needs the daemon's user in the `input`
group (the same `shepherd install groups` / login step as above); `/dev/uinput`
is **not** required. If the daemon can't read `/dev/input` the gate fails open
— gated activities stay visible and a warning is logged — so a missing group
never silently hides content.

## Kiosk hardening (optional)

Kiosk hardening is optional and intended for devices primarily used by
children, not developer machines.

```sh
sudo ./scripts/shepherd harden apply --user kiosk
```

This restricts the user to only access the Shepherd session by:
- Denying SSH access
- Restricting console (TTY) login
- Denying sudo access
- Restricting shell to Sway sessions only

To revert (all changes are reversible):
```sh
sudo ./scripts/shepherd harden revert --user kiosk
```

## Troubleshooting

### BLE management doesn't advertise (companion can't find the device)

If the companion app never sees the device in its pairing list and the
journal shows:

```
bluetoothd: src/advertising.c:add_client_complete() Failed to add advertisement: Invalid Parameters (0x0d)
```

the host Bluetooth stack is failing to register *any* LE advertisement,
so shepherd's management service never goes on air. Confirm whether the
controller itself works by comparing the legacy vs. bluetoothd paths:

```sh
# In one terminal:
sudo btmon
# In another — legacy MGMT path (should succeed):
sudo btmgmt add-adv -c 1
# bluetoothd's path (the one shepherd uses):
bluetoothctl advertise peripheral
```

If `btmgmt add-adv` succeeds but `bluetoothctl advertise peripheral`
fails, and `btmon` shows `Add Extended Advertising Data (0x0055) →
Invalid Parameters (0x0d)`, this is a **kernel extended-advertising
bug**, not a shepherd or controller problem — observed on Ubuntu 26.04's
`7.0.0-28-generic` across multiple Intel controllers (both BT 4.2 and
BT 5). bluetoothd always uses the extended path when the kernel exposes
it, and there is no config to force the legacy path. It matches the
mainline 6.18 regression in
[raspberrypi/linux#7473](https://github.com/raspberrypi/linux/issues/7473)
(a length-accounting bug in the MGMT `Add Extended Advertising Data`
handler).

Remediation is kernel-level. On Ubuntu 26.04 it regressed **between
`7.0.0-27.27` (works) and `7.0.0-28.28` (broken)**, so pin the last-good
kernel until a fixed one ships:

```sh
# reinstall 7.0.0-27 if it was autoremoved:
sudo apt install linux-image-7.0.0-27-generic linux-modules-7.0.0-27-generic \
                 linux-modules-extra-7.0.0-27-generic
# keep it, and stop newer broken ABIs from becoming the default:
sudo apt-mark hold linux-image-7.0.0-27-generic linux-modules-7.0.0-27-generic \
                   linux-generic linux-image-generic
# boot it by default:
sudo sed -i 's/^GRUB_DEFAULT=.*/GRUB_DEFAULT="Advanced options for Ubuntu>Ubuntu, with Linux 7.0.0-27-generic"/' /etc/default/grub
sudo update-grub
```

Holding the kernel pauses kernel security updates, so unpin
(`apt-mark unhold …`) once a fixed kernel is available — track that via
the Ubuntu bug
[LP #2161852](https://bugs.launchpad.net/ubuntu/+source/linux/+bug/2161852).
Swapping the Bluetooth adapter does **not** help — a BT 5 controller
fails the same way. (LL Privacy, advertising name length, and instance limits were
investigated and are *not* the cause; see
<docs/ai/history/2026-07-26 001 ble-advertisement-name-overflow.md>.)

## Complete documentation

See the scripts' [README](../scripts/README.md) for more.

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

`shepherd-admin media-deps install` does this together with the VA-API drivers
below, which is usually what you want on a machine that plays video.

### Hardware video decoding

`shepherd-media` decodes video on the GPU through mpv's VA-API support, which
needs a libva driver for your graphics hardware. Ubuntu's `mpv` package neither
depends on nor recommends one, so a machine that has never had one installed
decodes every frame on the CPU — several times the power draw, and not fast
enough for 1080p on older hardware.

`shepherd deps install run` installs the drivers for you. If you installed from
the `.deb` instead:

```sh
sudo shepherd-admin media-deps install   # VA-API drivers + yt-dlp
```

Or, for the drivers alone — `detect` reports the graphics hardware it finds and
the packages that match it, without changing anything:

```sh
shepherd-admin va-api detect
sudo shepherd-admin va-api install
```

`shepherd-media` reports what it ended up doing at the start of every video, so
its log tells you whether this worked:

```
INFO  mpv is decoding video with vaapi (zero-copy)
WARN  mpv is decoding video in software; playback will be CPU-bound. …
```

A warning here does not always mean a missing driver: fixed-function decoders
only cover certain codecs, so a GPU with no VP9 or AV1 block still decodes those
on the CPU. `shepherd-media` asks YouTube for H.264 first for exactly that
reason.

To install an activity backend (Steam via Canonical's snap, Chrome via Flathub,
RetroArch from the distro's own packages — matching what shepherd's
`type = "steam"`, `kind = "flatpak"` and `type = "retroarch"` adapters drive):

```sh
sudo shepherd-admin apps install steam    # or: chrome
```

`apps install retroarch` takes the libretro cores to install, named the way an
entry's `core =` field names them, and defaults to `mgba`:

```sh
sudo shepherd-admin apps install retroarch            # just mgba
sudo shepherd-admin apps install retroarch mgba nestopia snes9x
sudo shepherd-admin apps install retroarch help       # list the available cores
```

Cores come from apt, never from RetroArch's built-in core downloader, which
fetches unsigned binaries at runtime — not something a supervised kiosk should
do behind the operator's back. The Ubuntu archive packages 14 of them; the rest
(N64, GameCube/Wii, Saturn, arcade, ~85 more) are packaged only by the libretro
team's PPA, which `--ppa` opts into:

```sh
sudo shepherd-admin apps install retroarch --ppa mupen64plus-next
```

That adds a third-party apt source for the whole system, which is why it is
opt-in; `sudo add-apt-repository --remove ppa:libretro/testing` reverts it.

**No games are installed** — supply your own, and only ones you have the right
to. See [emulators.md](./emulators.md) for configuring an activity, where saves
live, and how the reset button works.

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

### With `shepherd-admin` (does the sideload for you)

`shepherd-admin` can fetch and install either app onto an Android device
attached over `adb` — useful for Fire TV sticks, where F-Droid is not a
practical route:

```sh
shepherd-admin apps install companion    # or: media
```

Run it **without `sudo`**: `adb` authorises devices against the invoking user's
key, so under `sudo` the device reports `unauthorized`.

Where the APK comes from follows how shepherd itself was installed. From the
`.deb` it downloads the release asset matching the installed version and checks
it against the release's published `.sha256`; from a source checkout it builds
the app's Gradle project instead, so you install what you just wrote. Either can
be forced with `--release` / `--source`.

| Option | Effect |
|---|---|
| `--device SERIAL` | Which device to install onto (default: the only one attached). A `HOST:PORT` serial is `adb connect`ed first, for a TV stick reached over the network |
| `--version X.Y.Z` | Download that release instead of the installed version |
| `--apk PATH` | Install a specific APK file |

`adb` and `curl` are `Suggests:` of the `.deb`, not `Depends:` — a kiosk that
never has a phone plugged into it should not carry the Android platform tools,
so `apt` does not install them by default. Both are checked when you run the
command, which prints the apt line to fix it:

```sh
sudo apt install adb          # curl is already present on stock Ubuntu images
```

The copy of `adb` in `/opt/android-sdk/platform-tools` that
`shepherd deps install android` provides is found automatically, so a build host
needs nothing extra.

A locally built APK is debug-signed, so it cannot replace a release- or
F-Droid-installed copy in place. `shepherd-admin` says so and prints the
`adb uninstall` that would be needed first — for the companion app that erases
its admin records and claim tokens, which means factory-resetting every device
it administers (see [Re-pairing](#re-pairing)), so read the warning before
following it.

## Pairing your phone with a device

Pairing is how a phone becomes the device's admin. The first phone to pair
claims the device (trust-on-first-use); after that the device refuses to pair
with anyone else until it is factory-reset, so there is no race to win and
nothing to configure.

You need the device's TV on and visible — pairing shows a six-digit code there
that you compare against your phone. Bluetooth must be on, and the device must
have `[service.ble_management] enabled = true` (the default).

1. Open **Shepherd Companion** and tap **Pair a device**.
2. Give the phone a name you will recognise later — it is what the device
   records as its admin, and what you will see if you ever need to check who
   claimed it.
3. Pick your device from the list. Devices advertise as `shepherd` unless
   `device_name` says otherwise. If several are in range, set a distinct
   `device_name` per device so you can tell them apart.
4. **Compare the six-digit code.** The TV shows one, the phone shows one, and
   they must be identical. If they match, confirm on the phone — Android asks
   twice: a "Pairing request" notification, then a dialog with the digits and a
   **Pair** button. It is the dialog that completes pairing.
5. If the codes do **not** match, cancel. A mismatch means the phone is talking
   to something other than the device in front of you, which is exactly what
   the comparison exists to catch.

Confirm within about half a minute — Bluetooth abandons the attempt after that
and you will have to start again.

The phone then claims the device and shows its activities. That phone is now
the admin, over Bluetooth and (once configured) over the network with the same
identity.

### Re-pairing

If the app says **Bond lost — re-pair needed**, the device no longer recognises
the phone — normally because it was factory-reset or its Bluetooth pairing was
removed. Tap **Re-pair** and repeat the steps above.

To hand a device to a different phone, or to recover when no phone can
administer it, factory-reset the management state on the device itself:

```sh
sudo touch /var/lib/shepherdd/.factory-reset-ble
sudo reboot
```

The reset is applied at startup, and `shepherdd` runs as part of the kiosk
session rather than as a system service — so a reboot (or signing out of the
kiosk session and back in) is what applies it. The file is consumed in the
process, so this happens once rather than on every boot.

That clears the admin record and the Bluetooth bond and returns the device to
unclaimed, so the next phone to pair claims it. The old phone's stored
credentials stop working; remove the stale pairing on that phone from Android's
Bluetooth settings.

Adjust the path if you set `admin_record_path`/`reset_sentinel_path` — the
sentinel lives in the configured `data_dir`.

If the device never appears in the pairing list at all, it is not advertising —
see "BLE management doesn't advertise" under Troubleshooting below.

### Keeping the device from dialling the phone

A system install adds a small drop-in,
`/etc/systemd/system/bluetooth.service.d/10-shepherd-bluetooth-experimental.conf`,
which runs `bluetoothd -E`. It exists for one reason, and it is worth
understanding before you remove it.

Pairing over LE with a dual-mode adapter also mints a BR/EDR key, so the phone
lands in the device's bond store looking like an ordinary audio device. BlueZ
then arms the *kernel* to auto-connect it whenever it advertises. That is
backwards for shepherd: the companion is the client and the device is a GATT
peripheral, so a link the device originates puts the device in the central
role — and only a central can start encryption. The phone can never encrypt
such a link, so every read fails with `Insufficient Authentication`, on a
connection that never drops. Symptomatically the companion sits on "Can't reach
this device securely" and nothing on the device side fixes it, including
restarting the session.

shepherd prevents this by setting `PreferredBearer=bredr` on the paired phone,
which stops BlueZ arming the kernel while leaving BR/EDR auto-connect
(headphones, controllers) alone. That property is flagged experimental
upstream, hence `-E`.

Without the drop-in nothing breaks outright: `shepherdd` logs how to enable the
property and falls back to dropping links that go unused, which costs a delay
on each occurrence. To apply it by hand instead — or on a box where you would
rather not enable experimental interfaces — write the setting straight into the
bond record with `bluetoothd` stopped:

```sh
sudo systemctl stop bluetooth
# /var/lib/bluetooth/<adapter>/<phone>/info, under [General]
sudo sed -i '/^\[General\]/a PreferredBearer=bredr' \
    /var/lib/bluetooth/AA:BB:CC:DD:EE:FF/11:22:33:44:55:66/info
sudo systemctl start bluetooth
```

BlueZ loads and honours the stored value regardless of `-E`; the flag only
governs whether the property can be *set* over D-Bus.

A device with a Bluetooth radio it does not share is better off still: give
shepherd a dedicated USB adapter, turn BR/EDR off on it (`btmgmt bredr off`)
and pin it with `[service.ble_management] adapter`. No BR/EDR means no
cross-transport key, so the phone never looks like an audio device and nothing
has a reason to dial it.

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
- A `bluetoothd` drop-in enabling experimental D-Bus interfaces, which
  shepherd needs to stop the device auto-dialling the paired phone (see
  "Keeping the device from dialling the phone" above)
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

### The compositor socket is closed after startup (issue #144)

Once `shepherdd` has connected to sway it **unlinks the compositor's IPC
socket**, so no other process can reach it for the rest of the session. This is
the default, and the installed `/etc/sway/shepherd.conf` simply does not opt
out of it.

This is not a micro-optimisation. Sway's IPC grants every process running as
shepherdd's own uid — which is every activity — the whole compositor:
`exec` starts a process outside shepherd's supervision *and* outside the cgroup
the per-entry firewall is attached to, so an activity configured `default_deny`
could ask sway to make its network requests for it. `exit` ends the kiosk
session; `kill` closes the HUD. Sway has no access control to turn on, and file
permissions cannot help while activities share shepherdd's uid.

Consequences worth knowing before you debug a device:

- **`swaymsg` does not work in a kiosk session.** Neither does anything else
  that speaks sway IPC. `$SWAYSOCK` still points at the path, but there is
  nothing there.
- **shepherdd cannot be restarted inside a session.** A second one would have
  no socket to connect to. Log out and back in, or reboot.
- **If hardening fails, the session still starts.** An unhardened kiosk beats a
  child staring at a dead screen, so every failure path leaves the socket
  reachable and carries on. It is not silent: the device raises the `Critical`
  diagnostic `compositor_not_hardened`, visible in the web UI and the companion
  app, and the daemon's log carries the underlying reason.
- To get a socket back for one session, add a drop-in that passes
  `--sway-ipc-alias <path>` (a second name for the socket, created *before* the
  original is removed; it must be inside `$XDG_RUNTIME_DIR`, because it is a
  hard link) or `--no-harden-sway-ipc` to switch it off entirely. Both weaken
  the device for as long as they are in place.

**Running `shepherdd` by hand inside your own desktop will take your desktop's
sway socket away**, because the unlink applies to whatever session shepherdd is
in. Every development entry point in the repo passes `--no-harden-sway-ipc`
already — `sway.conf`, `shepherd dev headless`, `run-dev`, and the e2e harness —
so this only bites an invocation written from scratch. `shepherd install
sway-config` is what strips the flag back out for a device.

### shepherd's own socket accepts only the session and root (issue #144)

The other half of the same problem. `shepherdd` listens on a Unix socket for the
launcher, the HUD and the compositor's keybinding one-shots — and every activity
runs as `shepherdd`'s uid, so the socket's file permissions separate nothing.
Without a check, a game can call `stop_current`, `launch` another entry, or
`logout`.

A shared uid also rules out the fixes that suggest themselves. The socket's name
cannot be removed the way sway's is, because those one-shot clients connect by
path all session long. A token cannot help either: any secret a trusted client
can read — from its environment, its argv, or a file — an activity at the same
uid can read too.

What does work is the peer's **cgroup**, which the kernel maintains, every
descendant inherits, and an unprivileged process can neither forge nor leave.
`shepherdd` accepts a connection only from:

- a process in **its own cgroup** — that is the session: sway, the launcher, the
  HUD, the one-shots sway starts for a keybinding, and shepherd's own helper
  subprocesses (the input-compat sidecars, `wl-mirror`, the pairing overlay, and
  the short-lived commands it runs to read volume, brightness and audio state);
  and
- **root**, so `sudo` still reaches the daemon from an operator's own shell.

`yt-dlp` is the one helper deliberately kept *out* of that cgroup: it runs on a
background prefetch timer and parses whatever a remote host returns, so it is
launched into a transient scope of its own like an activity.

Those helpers are also located from a fixed list of root-owned directories
rather than `$PATH`. This matters more than it sounds: GDM's PAM stack is
configured with `user_readenv=1`, so `~/.pam_environment` — a file the kiosk user
owns — sets the session's environment, and an activity that could steer `$PATH`
could have shepherd exec a binary of its choosing *inside shepherd's own
cgroup*. For the same reason `SHEPHERD_*_BIN` and `SHEPHERD_FIREWALL_HELPER` are ignored
unless `--trust-env-binaries` is passed, which no device does — and which is a
flag rather than an environment variable precisely so that
`shepherd install sway-config` can strip it and refuse to finish if the strip
did not take. None of the development opt-outs read the environment for the same
reason: an environment variable cannot be stripped from a config file.

Everything else is refused at accept, before it can read any state, and the
refusal raises the `ipc_peer_rejected` diagnostic naming the cgroup it came
from. Because an activity's cgroup is named after its session id, that usually
names the activity that went looking. Repeats are rate-limited to one report a
minute, carrying a count, so an activity cannot bury the warning by probing in a
loop.

For this to be a boundary, two things have to hold, and the daemon checks both:

- **Activities must be somewhere else.** They are: an entry with
  `[entries.firewall]` already gets a system-manager scope, snap and flatpak are
  scoped by their own runtimes, and everything else — Steam and the preloaded
  Steam client included — is launched into a transient scope of its own via
  `systemd-run --user --scope`.
- **The session's cgroup must be one an activity cannot join.** A session
  started from the installed *Shepherd Kiosk* desktop entry is in a logind
  session scope, which is root-owned — nothing at this uid can add a process to
  it. A session started from a shell, or from a `systemd --user` unit, is inside
  the user manager's delegated cgroups, where any process at this uid can join
  any cgroup, including `shepherdd`'s.

If either fails, the session still starts — an unhardened kiosk beats a dead one
— and the device raises the `Critical` diagnostic `ipc_socket_not_hardened`
saying which one. `--no-restrict-ipc-peers` turns the check off; like
`--no-harden-sway-ipc`, every development entry point passes it already, and
`shepherd install sway-config` strips it back out for a device.

**An activity can still take the socket's name.** It shares shepherd's uid, so
it can delete the socket file and bind its own in its place — no file permission
stops that, because a root-owned directory would stop `shepherdd` binding too,
and the sticky bit only restricts deletion to the file's owner, which an
activity is. What it cannot do is be *believed*: the launcher, the HUD and the
one-shots check the daemon's cgroup before they say anything to it, the same way
the daemon checks theirs. The device raises the `Critical` diagnostic
`ipc_socket_replaced` when it happens, and the session needs restarting.

**This does not close every path to the same effects.** The management HTTP API
and the BLE transport are separate surfaces with their own authentication, and
policy and usage state are files owned by the same uid the activities run as. An
activity that can read `config.toml` or `<data_dir>/admin.toml` is not stopped by
anything here. Those are tracked as #156 and #157.

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
assets, sway config, desktop entry, udev rule, bluetoothd drop-in). It deliberately leaves
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

Files that belong to an installed package are left alone, and `uninstall`
says so. Four of the files a from-source install places are shipped by the
`.deb` as *conffiles* — the sway config, the polkit rule, the udev rule and
the bluetoothd drop-in — and dpkg records a checksum for each. Deleting one
behind dpkg's back makes it read the absence as a deliberate admin removal,
so it will not restore the file, not even on `apt install --reinstall`.

#### Recovering a box with missing config

If a source uninstall ran before this behaviour existed, an apt install on
that box can silently come up missing those files. The symptoms are a
Shepherd session that appears in the greeter and drops straight back to it
(no `/etc/sway/shepherd.conf`) and an empty
`/etc/systemd/system/bluetooth.service.d/` (no drop-in, so the bearer pin
cannot apply). The polkit and udev rules go the same way, taking the
firewall helper and the input-compat sidecars with them.

Check which files dpkg expects and compare against what is on disk:

```sh
dpkg-query -W -f='${Conffiles}\n' shepherd-launcher
ls -l /etc/sway/shepherd.conf \
      /etc/systemd/system/bluetooth.service.d/ \
      /etc/polkit-1/rules.d/50-shepherd-firewall.rules \
      /etc/udev/rules.d/71-shepherd-uinput.rules
```

Restore the missing ones:

```sh
sudo apt install --reinstall -o Dpkg::Options::="--force-confmiss" shepherd-launcher
```

`--force-confmiss` is the override that tells dpkg to put back conffiles it
believes were removed on purpose. The postinst re-points the drop-in at this
machine's `bluetoothd` and restarts Bluetooth, so the pin applies without
further steps.

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

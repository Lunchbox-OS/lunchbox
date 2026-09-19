# Installation

Lunchbox can be installed on Linux with a modern Wayland compositor.
It is currently developed and tested on Ubuntu 26.04.

Lunchbox can be installed from the apt repository (the quick path,
with automatic upgrades), from a standalone prebuilt `.deb`, or from source (for
development). `./scripts/lunchbox` can help set up your build environment and
manage a source installation.

## Installing from the apt repository

Prebuilt amd64 packages are published to Lunchbox's own apt repository, so you
can install and then `apt upgrade` on future releases. Add the
repository's signing key and source list once (prereleases are deliberately not
published here, so `apt upgrade` only tracks stable versions):

```sh
sudo install -d -m 0755 /etc/apt/keyrings
sudo curl -fsSL https://apt.lunchbox-os.com/repository.key \
  -o /etc/apt/keyrings/lunchbox-os.asc
echo "deb [signed-by=/etc/apt/keyrings/lunchbox-os.asc] \
https://apt.lunchbox-os.com stable main" \
  | sudo tee /etc/apt/sources.list.d/lunchbox.list
sudo apt update
sudo apt install lunchbox-launcher
```

`apt` pulls in the runtime dependencies (Sway, mpv, BlueZ, …) from the Ubuntu
archive; the Lunchbox repository only carries `lunchbox-launcher` itself. Post-
install (package contents, per-user setup) is identical to the standalone `.deb`
below — continue with the `lunchbox-admin setup-user` step described there.

## Installing from a standalone `.deb`

If you'd rather not add the apt repository, prebuilt amd64 packages are also
attached to each
[release](https://github.com/aarmea/lunchbox/releases).
Download the `.deb` for the version you want and install it with `apt`, which
also pulls in the runtime dependencies (Sway, mpv, BlueZ, …):

```sh
sudo apt install ./lunchbox-launcher_0.2.0_amd64.deb
```

The package installs the binaries, the privileged firewall helper and its
polkit assets, the `/dev/uinput` udev rule, the Sway kiosk session, and the
display-manager session entry. Its post-install step creates the
`lunchbox-firewall` system group and reloads udev/polkit.

A distro package can't know which account is your kiosk user, so per-user setup
is **not** done automatically. The package ships a `lunchbox-admin` CLI for the
post-install admin tasks (the same code the from-source `./scripts/lunchbox`
runs). Deploy the example config and add the user to Lunchbox's groups with one
command (substitute your user for `kiosk`):

```sh
sudo lunchbox-admin setup-user kiosk
```

If you use YouTube media libraries, also install `yt-dlp`. Lunchbox deliberately
does not use the apt `yt-dlp` — YouTube changes formats often and the archived
build goes stale — so it lives in a venv you can refresh independently:

```sh
sudo lunchbox-admin yt-dlp install   # re-run periodically to update
```

`lunchbox-admin media-deps install` does this together with the VA-API drivers
below, which is usually what you want on a machine that plays video.

### Hardware video decoding

`lunchbox-media` decodes video on the GPU through mpv's VA-API support, which
needs a libva driver for your graphics hardware. Ubuntu's `mpv` package neither
depends on nor recommends one, so a machine that has never had one installed
decodes every frame on the CPU — several times the power draw, and not fast
enough for 1080p on older hardware.

`lunchbox deps install run` installs the drivers for you. If you installed from
the `.deb` instead:

```sh
sudo lunchbox-admin media-deps install   # VA-API drivers + yt-dlp
```

Or, for the drivers alone — `detect` reports the graphics hardware it finds and
the packages that match it, without changing anything:

```sh
lunchbox-admin va-api detect
sudo lunchbox-admin va-api install
```

`lunchbox-media` reports what it ended up doing at the start of every video, so
its log tells you whether this worked:

```
INFO  mpv is decoding video with vaapi (zero-copy)
WARN  mpv is decoding video in software; playback will be CPU-bound. …
```

A warning here does not always mean a missing driver: fixed-function decoders
only cover certain codecs, so a GPU with no VP9 or AV1 block still decodes those
on the CPU. `lunchbox-media` asks YouTube for H.264 first for exactly that
reason.

To install an activity backend (Steam via Canonical's snap, Chrome via Flathub,
RetroArch and Okular from the distro's own packages — matching what Lunchbox's
`type = "steam"`, `kind = "flatpak"`, `type = "retroarch"` and `type = "ebook"`
adapters drive):

```sh
sudo lunchbox-admin apps install steam    # or: chrome
```

`apps install retroarch` takes the libretro cores to install, named the way an
entry's `core =` field names them, and defaults to `mgba`:

```sh
sudo lunchbox-admin apps install retroarch            # just mgba
sudo lunchbox-admin apps install retroarch mgba nestopia snes9x
sudo lunchbox-admin apps install retroarch help       # list the available cores
```

Cores come from apt, never from RetroArch's built-in core downloader, which
fetches unsigned binaries at runtime — not something a supervised kiosk should
do behind the operator's back. The Ubuntu archive packages 14 of them; the rest
(N64, GameCube/Wii, Saturn, arcade, ~85 more) are packaged only by the libretro
team's PPA, which `--ppa` opts into:

```sh
sudo lunchbox-admin apps install retroarch --ppa mupen64plus-next
```

That adds a third-party apt source for the whole system, which is why it is
opt-in; `sudo add-apt-repository --remove ppa:libretro/testing` reverts it.

**No games are installed** — supply your own, and only ones you have the right
to. See [emulators.md](./emulators.md) for configuring an activity, where saves
live, and how the reset button works.

`apps install okular` sets up reading activities (`type = "ebook"`):

```sh
sudo lunchbox-admin apps install okular
```

It installs three packages, and the second is the one people miss: `okular`
itself, `okular-extra-backends` — EPUB and DjVu support ship separately from
Okular on Ubuntu, so without it a reading activity opens PDFs and refuses
novels — and `fonts-noto-core` for the default reading font. Lunchbox generates
the reader's whole configuration per entry and re-renders it on every launch,
so there is nothing to set up by hand.

**No books are installed.** Supply your own, DRM-free; a book from a store
belongs in that store's app (a browser or Android activity). See
[ebooks.md](./ebooks.md) for configuring an activity, where reading positions
live, and what the restrictions do and do not cover.

`apps install steam` also connects the snap's `mount-observe` interface and
permits unprivileged user namespaces (`kernel.apparmor_restrict_unprivileged_userns=0`
via `/etc/sysctl.d/90-lunchbox-userns.conf`) — Steam's sandbox needs one, and
Ubuntu 23.10+ restricts them by default, otherwise Steam fails with "Steam now
requires user namespaces to be enabled." This relaxes that hardening
system-wide; remove the drop-in and reboot to revert.

To make the hardware power button sleep the device instead of shutting it down
(a long press still powers off):

```sh
sudo lunchbox-admin power-key suspend
```

`lunchbox-admin` also exposes `harden` (kiosk lockdown) and `bluetooth clear`
(unpair the device's admin phone and return it to unclaimed); run
`lunchbox-admin --help` for the full list.

Then have `kiosk` log out and back in (so the new group memberships take
effect) and pick the "Lunchbox Kiosk" session at login. Then run
[kiosk hardening](#kiosk-hardening) — it is what two of Lunchbox's own
protections rest on, not just a lockdown preference.

> The companion `.apk` is attached to the same release; see
> [Installing the Android apps](#installing-the-android-apps) below.

## Installing the Android apps

Two Android apps ship alongside the launcher:

- **Lunchbox Companion** — the parent-facing admin app. Pairs with a device over
  Bluetooth LE and drives the management RPCs. This is the one you want.
- **Lunchbox Media** — the media player, for phones, tablets, and Fire TV sticks.

> **Coming from Shepherd Companion?** The project was renamed in 2026-09, and
> with it the apps' package names (`com.armeafamily.shepherd.*` →
> `com.lunchboxos.*`). Android treats a new package name as a *different app*,
> so this is not an upgrade:
>
> * Lunchbox Companion installs alongside Shepherd Companion instead of
>   replacing it, and starts empty — **your paired devices and their claim
>   tokens do not carry over, and cannot be exported.** You will pair each
>   device again, which needs physical access to compare the code on its TV.
> * Uninstall Shepherd Companion **first**. A device stops advertising while
>   any phone is connected to it, so the old app holding its bond is enough to
>   keep the new one from ever seeing the device.
> * If a device still refuses to appear, clear the stale Bluetooth bond on
>   *both* sides — Settings → the device → Forget on the phone, and
>   `sudo bluetoothctl remove <phone-address>` on the device. A bond that one
>   side has and the other does not will fail pairing in a way that looks like
>   a broken app.

### From the F-Droid repository (recommended, gives updates)

Install the [F-Droid](https://f-droid.org) client, then add the "Lunchbox
Apps" repository:

```
https://lunchbox-os.com/fdroid/repo?fingerprint=b3dc6194dca2d059c0714202b1ad11ec712581061e2be66b74a0b14abe15b422
```

The quickest way to get that onto a phone is to open
<https://lunchbox-os.com/fdroid/repo/> on the device and scan the QR code
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
[releases page](https://github.com/aarmea/lunchbox/releases)
and install it:

```sh
adb install lunchbox-companion_0.3.0.apk
```

Both APKs are signed with the same key across releases, so an `adb install` over
an existing install upgrades it in place.

### With `lunchbox-admin` (does the sideload for you)

`lunchbox-admin` can fetch and install either app onto an Android device
attached over `adb` — useful for Fire TV sticks, where F-Droid is not a
practical route:

```sh
lunchbox-admin apps install companion    # or: media
```

Run it **without `sudo`**: `adb` authorises devices against the invoking user's
key, so under `sudo` the device reports `unauthorized`.

Where the APK comes from follows how Lunchbox itself was installed. From the
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
`lunchbox deps install android` provides is found automatically, so a build host
needs nothing extra.

A locally built APK is debug-signed, so it cannot replace a release- or
F-Droid-installed copy in place. `lunchbox-admin` says so and prints the
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

1. Open **Lunchbox Companion** and tap **Pair a device**.
2. Give the phone a name you will recognise later — it is what the device
   records as its admin, and what you will see if you ever need to check who
   claimed it.
3. Pick your device from the list. Devices advertise as `lunchbox` unless
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
sudo touch /var/lib/lunchboxd/admin/.factory-reset-ble
sudo reboot
```

The reset is applied at startup, and `lunchboxd` runs as part of the kiosk
session rather than as a system service — so a reboot (or signing out of the
kiosk session and back in) is what applies it. The file is consumed in the
process, so this happens once rather than on every boot.

That directory belongs to the state custodian, so `sudo` is doing real work
here rather than being habit: the kiosk user cannot write it, which is the
point — a factory reset an activity could trigger would be a way for a game to
unpair the phone that supervises it (issue #157).

It has no kiosk user in the path because a factory reset is the *device's*, like
the Bluetooth bond it forgets: there is one adapter and one bond table, so one
admin record, shared by every kiosk user on the machine. On a device installed
before the custodian existed, the sentinel is
`~<kiosk-user>/.local/share/lunchboxd/.factory-reset-ble` instead — per user, as
the whole arrangement was then.

That clears the admin record and the Bluetooth bond and returns the device to
unclaimed, so the next phone to pair claims it. The old phone's stored
credentials stop working; remove the stale pairing on that phone from Android's
Bluetooth settings.

It deliberately leaves **web management access alone**: the web password is one
a parent chose, not the phone's, and a household whose phone broke would
otherwise be locked out of the device entirely. So after a factory reset the web
UI still signs in with the same password, browsers already signed in stay signed
in, and the login page simply stops offering "Approve on my phone" until a phone
is paired again. To clear the web side too, revoke the sessions from the web UI,
or start over with `lunchbox web-auth reset`.

If the device never appears in the pairing list at all, it is not advertising —
see "BLE management doesn't advertise" under Troubleshooting below.

### Keeping the device from dialling the phone

A system install adds a small drop-in,
`/etc/systemd/system/bluetooth.service.d/10-lunchbox-bluetooth-experimental.conf`,
which runs `bluetoothd -E`. It exists for one reason, and it is worth
understanding before you remove it.

Pairing over LE with a dual-mode adapter also mints a BR/EDR key, so the phone
lands in the device's bond store looking like an ordinary audio device. BlueZ
then arms the *kernel* to auto-connect it whenever it advertises. That is
backwards for Lunchbox: the companion is the client and the device is a GATT
peripheral, so a link the device originates puts the device in the central
role — and only a central can start encryption. The phone can never encrypt
such a link, so every read fails with `Insufficient Authentication`, on a
connection that never drops. Symptomatically the companion sits on "Can't reach
this device securely" and nothing on the device side fixes it, including
restarting the session.

Lunchbox prevents this by setting `PreferredBearer=bredr` on the paired phone,
which stops BlueZ arming the kernel while leaving BR/EDR auto-connect
(headphones, controllers) alone. That property is flagged experimental
upstream, hence `-E`.

Without the drop-in nothing breaks outright: `lunchboxd` logs how to enable the
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
Lunchbox a dedicated USB adapter, turn BR/EDR off on it (`btmgmt bredr off`)
and pin it with `[service.ble_management] adapter`. No BR/EDR means no
cross-transport key, so the phone never looks like an audio device and nothing
has a reason to dial it.

## Basic setup

The following builds and installs a fully functional local kiosk from source.

```sh
# 0. Install build dependencies
sudo ./scripts/lunchbox deps build run

# 1. Install runtime dependencies
sudo ./scripts/lunchbox deps install run

# 2. Build release binaries
./scripts/lunchbox build --release

# 3. Install everything for a kiosk user
sudo ./scripts/lunchbox install all --user kiosk
```

This installs:
- Binaries to `/usr/local/bin/`
- System Sway configuration to `/etc/sway/lunchbox.conf`
- Drop-in directory for Sway overrides at `/etc/sway/lunchbox.conf.d/`
- Display manager desktop entry ("Lunchbox Kiosk" session)
- A `bluetoothd` drop-in enabling experimental D-Bus interfaces, which
  Lunchbox needs to stop the device auto-dialling the paired phone (see
  "Keeping the device from dialling the phone" above)
- A udev rule at `/usr/lib/udev/rules.d/71-lunchbox-uinput.rules` and polkit
  rules at `/usr/share/polkit-1/rules.d/50-lunchbox-firewall.rules` and
  `50-lunchbox-session-guard.rules` — vendor directories, so a site override
  still goes in the matching `/etc` one (see "Who owns the files under /etc"
  below)
- The policy to `/var/lib/lunchboxd/state/kiosk/config.toml`, with a signpost at
  `~kiosk/.config/lunchbox/config.toml` saying where it went

`/etc/sway/lunchbox.conf` is regenerated by `lunchbox install sway-config`,
and replaced outright by an `apt upgrade`, so anything you want to keep across
upgrades belongs in `/etc/sway/lunchbox.conf.d/*.conf` instead. For example, to
scale a HiDPI display:

```sh
sudo tee /etc/sway/lunchbox.conf.d/hidpi.conf <<'EOF'
output * scale 2
EOF
```

See `man 5 sway-output` for the full set of `output` directives.

### The compositor socket is closed after startup (issue #144)

Once `lunchboxd` has connected to sway it **unlinks the compositor's IPC
socket**, so no other process can reach it for the rest of the session. This is
the default, and the installed `/etc/sway/lunchbox.conf` simply does not opt
out of it.

This is not a micro-optimisation. Sway's IPC grants every process running as
lunchboxd's own uid — which is every activity — the whole compositor:
`exec` starts a process outside Lunchbox's supervision *and* outside the cgroup
the per-entry firewall is attached to, so an activity configured `default_deny`
could ask sway to make its network requests for it. `exit` ends the kiosk
session; `kill` closes the HUD. Sway has no access control to turn on, and file
permissions cannot help while activities share lunchboxd's uid.

Consequences worth knowing before you debug a device:

- **`swaymsg` does not work in a kiosk session.** Neither does anything else
  that speaks sway IPC. `$SWAYSOCK` still points at the path, but there is
  nothing there.
- **lunchboxd cannot be restarted inside a session.** A second one would have
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

**Running `lunchboxd` by hand inside your own desktop will take your desktop's
sway socket away**, because the unlink applies to whatever session lunchboxd is
in. Every development entry point in the repo passes `--no-harden-sway-ipc`
already — `sway.conf`, `lunchbox dev headless`, `run-dev`, and the e2e harness —
so this only bites an invocation written from scratch. `lunchbox install
sway-config` is what strips the flag back out for a device.

### Lunchbox's own socket accepts only the session and root (issue #144)

The other half of the same problem. `lunchboxd` listens on a Unix socket for the
launcher, the HUD and the compositor's keybinding one-shots — and every activity
runs as `lunchboxd`'s uid, so the socket's file permissions separate nothing.
Without a check, a game can call `stop_current`, `launch` another entry, or
`logout`.

A shared uid also rules out the fixes that suggest themselves. The socket's name
cannot be removed the way sway's is, because those one-shot clients connect by
path all session long. A token cannot help either: any secret a trusted client
can read — from its environment, its argv, or a file — an activity at the same
uid can read too.

What does work is the peer's **cgroup**, which the kernel maintains, every
descendant inherits, and an unprivileged process can neither forge nor leave.
`lunchboxd` accepts a connection only from:

- a process in **its own cgroup** — that is the session: sway, the launcher, the
  HUD, the one-shots sway starts for a keybinding, and Lunchbox's own helper
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
could have Lunchbox exec a binary of its choosing *inside Lunchbox's own
cgroup*. For the same reason `LUNCHBOX_*_BIN` and `LUNCHBOX_FIREWALL_HELPER` are ignored
unless `--trust-environment` is passed, which no device does — and which is a
flag rather than an environment variable precisely so that
`lunchbox install sway-config` can strip it and refuse to finish if the strip
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
  started from the installed *Lunchbox Kiosk* desktop entry is in a logind
  session scope, which is root-owned — nothing at this uid can add a process to
  it. A session started from a shell, or from a `systemd --user` unit, is inside
  the user manager's delegated cgroups, where any process at this uid can join
  any cgroup, including `lunchboxd`'s.

If either fails, the session still starts — an unhardened kiosk beats a dead one
— and the device raises the `Critical` diagnostic `ipc_socket_not_hardened`
saying which one. `--no-restrict-ipc-peers` turns the check off; like
`--no-harden-sway-ipc`, every development entry point passes it already, and
`lunchbox install sway-config` strips it back out for a device.

**An activity can still take the socket's name.** It shares Lunchbox's uid, so
it can delete the socket file and bind its own in its place — no file permission
stops that, because a root-owned directory would stop `lunchboxd` binding too,
and the sticky bit only restricts deletion to the file's owner, which an
activity is. What it cannot do is be *believed*: the launcher, the HUD and the
one-shots check the daemon's cgroup before they say anything to it, the same way
the daemon checks theirs. The device raises the `Critical` diagnostic
`ipc_socket_replaced` when it happens, and the session needs restarting.

**This does not close every path to the same effects.** The management HTTP API
and the BLE transport are separate surfaces with their own authentication
(#156). Policy and state used to be reachable the same way; that is the section
below.

### Policy and state live at a uid activities do not have (issue #157)

The third door, and the one with the sharpest demonstration. `lunchboxd` runs as
the same uid as every activity it launches, and so did its files. Measured on an
installed device, an activity reset today's usage to zero, wiped the audit log,
appended an entry with `max_run_seconds = 0` to the policy, and launched it under
supervision with no deadline — the config is watched for changes, so the policy
edit took about three seconds and needed no restart.

File permissions cannot separate them while `lunchboxd` runs at that uid, so the
files move to one it does not have: a system user, `lunchbox-state`, owning
`/var/lib/lunchboxd/state/<user>/` at mode `0700`. It holds the database, the
policy, the BLE admin record and the factory-reset sentinel, and serves them to
`lunchboxd` over `/run/lunchboxd/state/<user>.sock` — a socket that accepts only
peers in the kiosk's logind session scope, a cgroup an activity can neither join
nor persuade logind to create another of.

```sh
systemctl status lunchbox-stated@kiosk.socket   # owns the name, always up
systemctl status lunchbox-stated@kiosk.service  # starts when lunchboxd connects
journalctl -u lunchbox-stated@kiosk.service     # what it trusted, what it refused
```

Installed by `lunchbox install all --user kiosk`, or on a packaged system by
`lunchbox-admin setup-user kiosk`. Both do the same per-user work — create the
protected directory, move the device's existing state into it, and enable the
socket — because none of it can happen at package time, when the kiosk user is
not known yet. `lunchbox-admin` is the packaged CLI and has no `install` verb, so `setup-user`
is the whole of it there — with `lunchbox-admin policy`, `migrate-state` and
`restore-state` covering the three per-device operations afterwards.

Consequences worth knowing before you debug a device:

- **The policy moved out of `~/.config/lunchbox/config.toml`.** Installing the
  custodian moves it to `/var/lib/lunchboxd/state/<user>/config.toml` and leaves
  a signpost at the old path saying so. There is exactly one policy file, and it
  is the one the daemon reads — no second copy to edit by mistake, and nothing
  to keep in step.

  The signpost is a valid policy that grants nothing, deliberately. lunchboxd
  reads it if the custodian ever cannot be reached, and what a device should do
  in that state is show no activities and report `state_not_protected`, not
  quietly run a policy nobody can see from where the custodian keeps it.

  Three ways to change what a device allows, all reloading within a second:

  ```sh
  sudoedit /var/lib/lunchboxd/state/kiosk/config.toml
  sudo lunchbox install policy --user kiosk --source ./new-config.toml
  sudo lunchbox-admin policy kiosk --source ./new-config.toml   # packaged
  ```

  ...and the **Config** tab in the web management UI (issue #185), which edits
  this same file graphically and writes it back over the management API. It is
  the only one of the three that needs neither a terminal nor a file, and it
  validates in the browser as you type.

  All but `sudoedit` validate the file before installing it; `sudoedit` does
  not, and a policy lunchboxd cannot parse is fatal at its next startup rather
  than on reload.
- **An administrator can reconfigure a device without touching the kiosk's home
  at all**, which is what a hardened device needs: `harden apply` gives the
  kiosk user `nologin` and denies it SSH, so there is no `su` into it to edit a
  config in `vim`.

  ```sh
  sudo lunchbox install policy --user kiosk --source ./new-config.toml
  ```

  That writes straight to the custodian, and is the same file `sudoedit` above
  opens and the web editor saves to — one policy, reachable three ways.

  **Both forms validate before they install.** A policy lunchboxd cannot parse
  is survivable on reload — it keeps the running one and logs — but fatal at
  startup, and since #172 a lunchboxd that exits takes the session down with
  it. So a typo would cost nothing until the next boot and then cost the whole
  session, on a device whose kiosk user has no shell to fix it from. A file
  that does not validate is reported and not installed, and the device keeps
  the policy it has.
- **The state is not in the user's home.** `/var/lib/lunchboxd/state/<user>/` is,
  and only `root` and `lunchbox-state` can read it.
- **The socket is created by systemd, not by the daemon**, which is what stops an
  activity taking its name the way it can with Lunchbox's own management socket
  (above).
- **If the custodian cannot be reached, the device does not start.** `lunchboxd`
  exits and the session ends at the login screen. That is deliberate, and it is
  the one place Lunchbox prefers a visible failure to a working-looking one: its
  state has *moved*, so carrying on would mean a fresh empty database, a claimed
  device presenting itself as unclaimed, and a launcher with no activities on it
  — which reads to a child like an ordinary evening with nothing available, and
  to an adult like the device is merely slow. The greeter says "something is
  wrong"; an empty grid does not. The journal carries the reason, and
  `systemctl status lunchbox-stated@<user>` is where to look.

  A device that has *no* custodian is different and still starts: nothing has
  moved, its state is where it always was, and it reports the `Critical`
  diagnostic `state_not_protected` rather than refusing over a protection it was
  never given.
- **The custodian also ends the session if `lunchboxd` stops supervising it**
  (issue #172). Every activity runs as the kiosk uid, and so does `lunchboxd`,
  so an activity can `kill` — or `SIGSTOP` — its own supervisor and carry on
  with no time accounting, no bedtime and no audit. The `sh -c` wrapper in the
  sway config catches a daemon that merely *exits*, but it runs at that same uid
  and can be killed first. The custodian cannot: it is outside the session, at a
  uid nothing inside it can signal.

  So `lunchboxd` holds a connection open to it and beats on it from the same
  loop that decides whether a child's time is up. Lose the connection and the
  session ends five seconds later; go quiet on it for a minute and the session
  ends too. Neither a suspend nor a slow boot counts as going quiet — the
  custodian stands down on logind's `PrepareForSleep` and allows two minutes for
  the first beat.

  Ending a session it does not own needs polkit, which is what
  `/usr/share/polkit-1/rules.d/50-lunchbox-session-guard.rules` grants (installed
  with the custodian, removed with it). Without that rule everything still works
  except the part that matters: the device raises the `Critical` diagnostic
  `session_not_guarded` and the journal names the file. **The rule grants
  `lunchbox-state` the right to end any session on the machine**, an
  administrator's SSH login included — polkit passes no details for
  `TerminateSession`, so it cannot be narrowed. The rule's own comments say so
  and say why it is worth it.

  Consequence for debugging: on a device you cannot pause or restart `lunchboxd`
  in place. Attaching a debugger that stops it ends the session, exactly as a
  child killing it would.
- **The `lunchbox-state` user and the state survive an uninstall.** Removing them
  would discard a device's usage history and its BLE admin record, which an
  uninstall is not entitled to do.
- **Hardening matters more than it used to.** The trusted cgroup is the kiosk's
  *graphical* session, so a second login for the same user is refused — but
  `lunchbox harden apply` is what stops that second login existing, and it also
  stops PAM reading a `~/.pam_environment` the kiosk user writes. See
  [Kiosk hardening](#kiosk-hardening).

#### Operating a device after the state moved

Three habits that worked before the custodian need adjusting. None of them
fails loudly, which is why they are written down here.

**Downgrading.** Migration *moved* the database and the BLE admin record out of
the kiosk user's home. A build older than this one looks for them there, finds
nothing, creates an empty database, and — with no `admin.toml` — reports itself
as **unclaimed**, so the next phone to pair claims the device. A downgrade would
look exactly like a factory reset. Move the state back first:

```sh
sudo lunchbox uninstall state --restore-to-home   # from source
sudo lunchbox-admin restore-state                 # packaged; apt removes the rest
```

That stops the custodian, returns each user's `lunchboxd.db` (with its SQLite
side files), `admin.toml` and policy to the home directory, and then removes the
binary and units. It never overwrites a real file already there — on that path
the home copy is the one the older build will read, so it wins and the
custodian's is left behind and named. The signpost is the one thing it does
write over: it exists to say the policy lives elsewhere, and after this it does
not.

**Backups.** A device's state is now in **two** places, and a backup of `/home`
alone no longer covers it:

| What | Where | Owner |
| --- | --- | --- |
| usage history, quotas, audit log, policy | `/var/lib/lunchboxd/state/<user>/` | `lunchbox-state` |
| BLE admin record, unbond queue, reset sentinel — the *device's*, not a user's | `/var/lib/lunchboxd/admin/` | `lunchbox-state` |
| RetroArch save states, ebook progress, media cache | `~<user>/.local/share/lunchboxd/` | the kiosk user |

Back up both, and preserve ownership (`tar --numeric-owner`, `rsync -a`). If you
restore onto a *different* machine, check the uid: `lunchbox-state` is created
with whatever system id was free, so it can differ between devices, and a
restore by number can land the state on the wrong name. `sudo chown -R
lunchbox-state:lunchbox-state /var/lib/lunchboxd/state` after restoring fixes it.

**Reading or editing the database by hand.** It is no longer in the kiosk user's
home, and `sudo sqlite3` is the wrong way to reach it:

```sh
# Wrong: root creates root-owned -wal/-shm in a directory lunchbox-state owns,
# and the custodian can then no longer write its own database.
sudo sqlite3 /var/lib/lunchboxd/state/kiosk/lunchboxd.db

# Right: become the uid that owns it.
sudo -u lunchbox-state sqlite3 /var/lib/lunchboxd/state/kiosk/lunchboxd.db
```

Stop the custodian first if you are writing (`sudo systemctl stop
lunchbox-stated@kiosk.service`); it holds the database open for as long as it
runs, and it starts again on the daemon's next connection. If you have already
left root-owned side files behind, `sudo chown lunchbox-state:lunchbox-state
/var/lib/lunchboxd/state/kiosk/lunchboxd.db*` puts it right.

### External monitor / docking (issue #87)

If you use external-monitor mirroring, the Sway session must be started with
`WLR_SCENE_DISABLE_DIRECT_SCANOUT=1` in its environment. Without it, a
fullscreen activity direct-scans-out and starves `wl-mirror`'s screen capture,
blacking the mirrored display. This is a compositor-startup environment variable
(read by wlroots, not settable from `sway.conf`), so set it wherever the kiosk
session is launched — e.g. in the "Lunchbox Kiosk" desktop entry's `Exec`, or a
drop-in `environment.d`/PAM env file for the kiosk user:

```sh
echo 'WLR_SCENE_DISABLE_DIRECT_SCANOUT=1' | sudo tee -a /etc/environment
```

Mirroring also requires the `wl-mirror` package (installed by
`lunchbox deps install run`).

For custom installation paths:
```sh
# Install to /usr instead of /usr/local
sudo ./scripts/lunchbox install all --user kiosk --prefix /usr
```

Individual installation steps can be run separately:
```sh
sudo ./scripts/lunchbox install bins --prefix /usr/local
sudo ./scripts/lunchbox install config --user kiosk
```

### Uninstalling

`lunchbox uninstall` reverses a from-source install, removing the files
`lunchbox install` placed system-wide (binaries, firewall helper + polkit
assets, sway config, desktop entry, udev rule, bluetoothd drop-in). It deliberately leaves
per-user config under `~/.config/lunchbox` and group memberships in place —
remove those by hand if you want them gone.

```sh
# Remove just the binaries (use the same --prefix you installed with)
sudo ./scripts/lunchbox uninstall bins --prefix /usr/local

# Remove everything installed system-wide
sudo ./scripts/lunchbox uninstall all

# ...and put the state back where a build without the custodian looks for it
sudo ./scripts/lunchbox uninstall all --restore-to-home
```

(If you installed the `.deb`, use `sudo apt-get remove lunchbox-launcher`
instead. Since issue #177 that takes the sway config and the udev and polkit
rules with it — they are Lunchbox's files, not conffiles the admin owns — while
leaving `/etc/sway/lunchbox.conf.d/` and the state custodian's data alone.)

The state custodian's own files are left in place by default, and so is the
`lunchbox-state` user that owns them, so a reinstall picks a device's history
straight back up. `--restore-to-home` is for the other case — going back to a
build that predates the custodian, which will not find the state there. See
[Operating a device after the state moved](#operating-a-device-after-the-state-moved).

Files that belong to an installed package are left alone, and `uninstall`
says so. On a packaged box that is most of what these commands would otherwise
remove, and dpkg's idea of what is installed goes stale the moment one of them
disappears behind its back.

#### Recovering a box with missing config

If a source uninstall ran before that behaviour existed, an apt install on
that box can silently come up missing those files. The symptoms are a
Lunchbox session that appears in the greeter and drops straight back to it
(no `/etc/sway/lunchbox.conf`), and a firewall helper and input-compat
sidecars that stop working (no polkit or udev rule).

Compare what dpkg thinks it installed against what is on disk:

```sh
dpkg-query -L lunchbox-launcher | xargs -d '\n' ls -ld 2>&1 | grep -i 'no such'
```

Restore the missing ones:

```sh
sudo apt install --reinstall lunchbox-launcher
```

The package declares no *conffiles* (issue #177), so a plain `--reinstall` puts
everything back; `--force-confmiss` is no longer needed. The bluetoothd drop-in
is not in dpkg's file list at all — the postinst writes it, aiming it at this
machine's `bluetoothd` and restarting Bluetooth — so a reinstall restores that
too.

#### Who owns the files under `/etc`

Nothing Lunchbox installs is a dpkg *conffile*: there is no file here whose
local edits an upgrade will preserve or ask you about. That is deliberate.

- `/etc/sway/lunchbox.conf` is **generated**, with the rewrites described under
  [The compositor socket is closed after startup](#the-compositor-socket-is-closed-after-startup-issue-144)
  verified as it is written. A preserved local copy would carry an unverified
  config across upgrades forever, which is why site config belongs in
  `/etc/sway/lunchbox.conf.d/*.conf` — left unmanaged on purpose — instead.
- `/etc/systemd/system/lunchbox-stated@.{service,socket}` are Lunchbox's units,
  pinned against the daemon's own constants by a test in the repository. A
  surviving local edit would break the state custodian quietly.
- `/etc/systemd/system/bluetooth.service.d/10-lunchbox-bluetooth-experimental.conf`
  is written by the postinst, because its `ExecStart` has to name *this*
  machine's `bluetoothd`.

The udev and polkit rules are not under `/etc` at all any more. They live at
`/usr/lib/udev/rules.d/71-lunchbox-uinput.rules`,
`/usr/share/polkit-1/rules.d/50-lunchbox-firewall.rules` and
`/usr/share/polkit-1/rules.d/50-lunchbox-session-guard.rules`, next to the polkit
action Lunchbox has always installed to `/usr/share/polkit-1/actions/`. Both
subsystems read their `/etc` directory as well, and a same-named file there
wins — so that is still where a site override goes, it is simply no longer
where Lunchbox's own copy sits.

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

**The HUD needs the same `/dev/uinput` access**, whether or not any activity
configures a sidecar: its page-turn buttons for `type = "ebook"` activities
synthesize a keypress through it. `lunchbox install all` and the `.deb` both
provide it already, so a normal install needs nothing extra — but on an install
where the udev rule or the group was skipped deliberately, those buttons do
nothing, and a paged reading activity on a touch-only device raises the
`ebook_no_page_turn` diagnostic saying so.

`lunchbox install all` adds the group and installs the udev rule
automatically. To set them up on an existing install, run:

```sh
sudo ./scripts/lunchbox install groups --user kiosk
sudo ./scripts/lunchbox install udev
```

The group change takes effect on the user's next login; the udev rule applies
after the next `/dev/uinput` access (or a reboot).

## Input-device dependencies (optional)

Activities can also declare a hardware dependency via `requires_input` (see
`config.example.toml`) — for example a typing tutor that should only appear
once a physical keyboard is connected. Supported types are `mouse`, `touch`,
`keyboard`, and `gamepad`.

Unlike the sidecars above, this is enforced by `lunchboxd` itself: it reads
`/dev/input/event*` to see which device types are attached and hides gated
activities until they are. It therefore needs the daemon's user in the `input`
group (the same `lunchbox install groups` / login step as above); `/dev/uinput`
is **not** required. If the daemon can't read `/dev/input` the gate fails open
— gated activities stay visible and a warning is logged — so a missing group
never silently hides content.

## Kiosk hardening

Intended for devices used by children rather than developer machines — but on
such a device it is **not optional**, because two of Lunchbox's own protections
depend on it:

- It strips `user_readenv=1` from `/etc/pam.d`, which is what stops PAM handing
  the session an environment the kiosk user wrote. Without it, an activity can
  put `~/.pam_environment` in place and choose the next session's `PATH` and
  variables — including where Lunchbox looks for its state (issue #144).
- It denies the kiosk user SSH and console login, which is what stops a second
  session existing. The state custodian trusts the kiosk's *graphical* session
  specifically, and refuses any other (issue #157).

An unhardened device still runs; it just runs with those two doors open.

```sh
sudo ./scripts/lunchbox harden apply --user kiosk
```

This restricts the user to only access the Lunchbox session by:
- Denying SSH access
- Restricting console (TTY) login
- Denying sudo access
- Restricting shell to Sway sessions only

To revert (all changes are reversible):
```sh
sudo ./scripts/lunchbox harden revert --user kiosk
```

## Troubleshooting

### BLE management doesn't advertise (companion can't find the device)

If the companion app never sees the device in its pairing list and the
journal shows:

```
bluetoothd: src/advertising.c:add_client_complete() Failed to add advertisement: Invalid Parameters (0x0d)
```

the host Bluetooth stack is failing to register *any* LE advertisement,
so Lunchbox's management service never goes on air. Confirm whether the
controller itself works by comparing the legacy vs. bluetoothd paths:

```sh
# In one terminal:
sudo btmon
# In another — legacy MGMT path (should succeed):
sudo btmgmt add-adv -c 1
# bluetoothd's path (the one Lunchbox uses):
bluetoothctl advertise peripheral
```

If `btmgmt add-adv` succeeds but `bluetoothctl advertise peripheral`
fails, and `btmon` shows `Add Extended Advertising Data (0x0055) →
Invalid Parameters (0x0d)`, this is a **kernel extended-advertising
bug**, not a Lunchbox or controller problem — observed on Ubuntu 26.04's
`7.0.0-28-generic` across multiple Intel controllers (both BT 4.2 and
BT 5). bluetoothd always uses the extended path when the kernel exposes
it, and there is no config to force the legacy path. It matches the
mainline 6.18 regression in
[raspberrypi/linux#7473](https://github.com/raspberrypi/linux/issues/7473)
(a length-accounting bug in the MGMT `Add Extended Advertising Data`
handler).

**This was fixed in `7.0.0-31-generic`** (verified 2026-09-04: both a
Realtek 5.4 dongle and a Qualcomm 5.3 controller register D-Bus
advertisements again, and a companion pairing completes end to end). Only
`7.0.0-28` through `-30` are affected. If you are on one of those, either
upgrade to `-31` or later — the simple fix — or pin the last-good kernel,
which on Ubuntu 26.04 regressed **between `7.0.0-27.27` (works) and
`7.0.0-28.28` (broken)**:

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

Holding the kernel pauses kernel security updates, so if a host is still
pinned, unpin it now that `-31` is out:

```sh
sudo apt-mark unhold linux-image-7.0.0-27-generic linux-modules-7.0.0-27-generic \
                     linux-generic linux-image-generic
sudo sed -i 's/^GRUB_DEFAULT=.*/GRUB_DEFAULT=0/' /etc/default/grub
sudo update-grub
```

The upstream history is in the Ubuntu bug
[LP #2161852](https://bugs.launchpad.net/ubuntu/+source/linux/+bug/2161852).
Swapping the Bluetooth adapter does **not** help — a BT 5 controller
fails the same way. (LL Privacy, advertising name length, and instance limits were
investigated and are *not* the cause; see
<docs/ai/history/2026-07-26 001 ble-advertisement-name-overflow.md>.)

## Complete documentation

See the scripts' [README](../scripts/README.md) for more.

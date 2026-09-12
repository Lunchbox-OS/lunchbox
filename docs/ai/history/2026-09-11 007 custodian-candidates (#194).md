# What else belongs in the state custodian — a survey

**Prompt:** "the custodian makes sense for this -- check to see if there are
other features that are better implemented in the custodian rather than as the
child user"

Follow-up to `2026-09-11 006 wifi-configuration-scope (#194).md`, where #194's writes
to NetworkManager were routed through `shepherd-stated` rather than handing
`settings.modify.system` to the kiosk user. This surveys everything else
shepherd does at the kiosk user's uid, and asks the same question of each item.

This is an investigation, not an implementation. Nothing that changes device
state was exercised; the BlueZ probe below was read-only.

## What makes a feature a custodian feature

Properties the custodian has (`crates/shepherd-stated/README.md`,
`dist/systemd/shepherd-stated@.service`):

* It runs as a uid the activities don't have, and accepts only the kiosk
  session's cgroup.
* It already holds device-wide state (`/var/lib/shepherdd/admin`), not only
  per-child state.
* It talks to the system bus, which is `AF_UNIX`.
* It has **no network** (`PrivateNetwork=yes`, `RestrictAddressFamilies=AF_UNIX`),
  **no capabilities**, and **spawns nothing**. clippy denies a bare
  `Command::new`.
* It deserializes input only from a peer it has already accepted.

So a feature fits when all three of these hold:

1. **It guards an authority an activity must not have, and a uid is the gate**:
   a D-Bus policy, a polkit rule or a file mode. Anything that admits
   `shepherdd`'s uid admits every activity.
2. **It can be done over the system bus or with files**: no network sockets, no
   device nodes, no child processes, no capabilities.
3. **Its input comes from `shepherdd`**, or the cost of parsing untrusted input
   is accepted explicitly.

A feature that fails (2) may still want a uid of its own, just not this one.

## Summary

| capability | an activity can today | fits the custodian? | recommendation |
| --- | --- | --- | --- |
| WiFi configuration (#194) | — (not built) | yes | decided |
| **BlueZ**: pairing agent, pairable, bond removal, GATT/advertising registration | make the same calls; every uid is admitted | **yes, at a real cost** | measure the attack first; the most serious item |
| **Browser URL policy file** | rewrite the file that holds the allowlist | **yes** | move; small |
| RetroArch / Okular generated config | race the regeneration | partially | only together with the browser policy |
| `shepherdd`'s IPC socket name | take the name, making the session unreachable (already detected) | yes, via fd passing | medium-low |
| HTTP management listener | squat the port | possible, awkward | not now |
| Firewall helper (`pkexec`) | run its own command in a fresh, unfirewalled scope | **no** | a firewall issue, not a custodian one |
| yt-dlp + media cache | steer downloads through yt-dlp's config and plugins | no (needs network) | two flags now; its own uid if cache integrity matters |
| Input bridges (`input`), brightness (`video`) | read/inject input, set the backlight | no (device nodes) | leave |

## 1. BlueZ — the strongest candidate, and the largest

### What was measured

* `/usr/share/dbus-1/system.d/bluetooth.conf` gives the default context
  `<allow send_destination="org.bluez"/>`. BlueZ installs no polkit actions on
  this host (`pkaction | grep -ci bluez` → `0`).
* As `shepherd-admin`, which is **not** in `bluetooth`, read-only: the adapter's
  `Pairable` and `Discoverable` read back fine. Introspection shows
  `AgentManager1.RegisterAgent` / `RequestDefaultAgent`,
  `Adapter1.RemoveDevice`, `GattManager1.RegisterApplication` and
  `LEAdvertisingManager1.RegisterAdvertisement`, all reachable under that
  policy.

So on 26.04 the `bluetooth` group isn't what grants access; every uid is
admitted. The comment at `scripts/lib/install.sh:470` ("BlueZ's polkit rules
grant the `bluetooth` group permission to call … SetPairable and …
RegisterAgent") does not describe this host.

### What the code does with a bond

* `ClaimMachine::authorize` (`crates/shepherd-ble/src/claim.rs:180`) returns
  `Allow` for **any** bonded peer once the device is claimed. That was relaxed
  for BlueZ identity-resolution drift. `2026-09-07 001` lists it as still open
  and hands it to #149.
* The Request characteristic requires `encrypt_authenticated_write`
  (`server.rs:1592`), i.e. a MITM-authenticated bond. That is exactly what an
  agent confirming a Numeric Comparison produces, whichever agent it was.

### The attack this suggests — NOT measured

1. An activity registers an agent that confirms everything, calls
   `RequestDefaultAgent`, and sets the adapter pairable and discoverable.
2. A phone the child controls pairs. The rogue agent confirms, and BlueZ stores
   an authenticated bond.
3. The phone writes JSON-RPC to the Request characteristic, using the companion
   (published on F-Droid) or any GATT tool. `authorize` allows it. That is full
   management: extend time, overrides, admin mode, `set_web_password`.

Lesser variants from the same open policy:

* `RemoveDevice` on the parent's phone locks management out.
* A second GATT application registered under shepherd's service UUID might
  receive what the parent's phone sends, a new web password included.

**Measure this on a device before acting on it.** The `companion-pairing` setup
has everything needed.

### Why this is the custodian's

The gate that can stop this is a D-Bus policy fragment in
`/etc/dbus-1/system.d/`. It would deny `AgentManager1`, `Adapter1.RemoveDevice`,
`GattManager1`, `LEAdvertisingManager1` and `Properties.Set` on `org.bluez` to
the kiosk user, and allow them to `shepherd-state`. That only works if the
custodian is the process making those calls.

The custodian's sandbox already fits it:

* `bluer` is built with `features = ["bluetoothd"]`, i.e. D-Bus only.
* GATT's write/notify file descriptors are `AF_UNIX` socketpairs.
* BLE is device-wide, like the admin record the custodian already holds.
* The open item "two sessions would contend for one adapter's GATT
  registration" goes away once one component owns the adapter.

### What it costs

* **The custodian would parse bytes from any radio in range**: the
  unauthenticated `DeviceInfo` read and the claim request. That breaks
  "deserialization happens only after acceptance". Keep what runs in the
  custodian to framing, claim and authorization, and forward authorized frames
  unchanged to `shepherdd` over the trusted connection. RPC dispatch stays
  where it is.
* **Per-user instances, one adapter.** It needs a rule for which instance owns
  the adapter: presumably the one whose session is active.
* **Admin mode loses Bluetooth settings.** `gnome-control-center` runs at the
  kiosk uid under #154, so pairing headphones would need its own management
  feature. A uid-keyed policy can't be relaxed for "while admin mode is on".
* It is by far the largest item here.

**Order:**

1. Measure the attack.
2. Do the part that doesn't need the custodian: authorize against the admin
   record's identity (IRK). #149 decides how that coexists with multiple bonds.
3. Move BLE authority into the custodian and ship the D-Bus policy.

## 2. The browser's URL policy file — small, clean fit

`write_policy_file` (`crates/shepherd-host-linux/src/browser.rs:127`) writes
`URLAllowlist`, `URLBlocklist`, `DeveloperToolsAvailability`,
`IncognitoModeAvailability` and `ExtensionInstallBlocklist` to
`~/.var/app/com.google.Chrome/config/shepherd-policies/<id>.json`. The flatpak
shim symlinks it into the sandbox's `/etc/opt/chrome/policies/managed/`
(`2026-06-13 007`).

That path is in the kiosk user's home, inside Chrome's own flatpak config
directory:

* Any activity can write it.
* It is regenerated at launch, so a write between sessions is undone.
* But Chrome watches its policy directory and reloads on change, so a write
  during a session takes effect. Chrome also drops a file it can't parse,
  together with its policies. *(Both are Chrome behaviour to verify on 1.54's
  flatpak.)*
* The policy sets neither `DownloadRestrictions` nor
  `AllowFileSelectionDialogs`. Whether Chrome's own save dialog can reach that
  path is worth one measurement.

**The fit:** the policy is a pure function of `config.toml`, which the custodian
already owns and watches. It can render each entry's policy into a directory it
owns that the kiosk uid can read but not write, and the shim mounts that with
`--filesystem=<dir>:ro`. No new privilege, no network, no spawn. One wrinkle:
the unit's `StateDirectoryMode=0700` applies to all its directories today, so a
readable one needs its own handling.

**A cheaper interim that doesn't touch the custodian:** set
`DownloadRestrictions` / `AllowFileSelectionDialogs` for entries that don't need
downloads. That is a product decision, not a fix.

## 3. Generated RetroArch and Okular config — only alongside §2

`retroarch::materialize` and `ebook::materialize` write the settings that make
an activity supervisable (`kiosk_mode_enable`, `config_save_on_exit`, the
save-state directories, Okular's settings) into kiosk-owned directories.
Both are regenerated at every launch, and `GUARDED_SETTINGS`
(`retroarch.rs:586`) already guards against RetroArch's own override files.

The remaining window is a write between materialization and the process reading
the file. That needs something already running outside the activity, which the
measured `systemd-run --user` escape (`2026-08-29 005`) provides. Once §2's
read-only directory exists, the fragment RetroArch reads with `--appendconfig`
can come from it cheaply. Okular's config directory is writable app state by
nature. Do this with §2 or not at all.

## 4. `shepherdd`'s IPC socket — the residual only socket activation fixes

Finding 2 of `2026-08-29 004` left a residual. An activity can still take the
name `$XDG_RUNTIME_DIR/shepherdd/shepherdd.sock`. Clients refuse the impostor
and `ipc_socket_replaced` is raised, but the session is unreachable, and
"preventing it requires the socket to be created by something other than the
daemon, i.e. systemd socket activation … which `shepherdd` cannot use while sway
`exec`s it."

The custodian closes exactly that gap:

* It is already socket-activated per user.
* Add a second `ListenStream=` in a root-owned `/run` directory to
  `shepherd-stated@.socket`.
* The custodian passes that listening fd to `shepherdd` over the connection it
  has already peer-checked (`SCM_RIGHTS`). `shepherdd` keeps its own peer check,
  and the name can no longer be taken.

Costs: the socket path clients use changes (headless dev and
`--no-state-custodian` keep today's path), and the protocol gains an fd-passing
request. The attack is denial of service and is already detected, so the
priority is medium-low.

## 5. The HTTP listener — possible, not now

The same trick could hand `shepherdd` a TCP socket bound by PID 1. A passed fd
doesn't care about the custodian's `PrivateNetwork`. That would stop an activity
squatting the management port during `bind_retry` or a restart and serving a
look-alike login page.

But `bind` and `port` live in `config.toml`, so the `.socket` unit would have to
be generated from the policy. And the look-alike can't present the stored
certificate (`ProtectedFile::TlsCert`), so a parent who pinned the fingerprint
would see a warning. The payoff doesn't justify it yet.

## Checked, and not the custodian's

* **Firewall helper** (`shepherd-firewall` group, `pkexec`). The polkit rule
  checks only the group, and the helper only checks `--uid == PKEXEC_UID`. So an
  activity can use it to run its own command in a fresh system scope with
  `--default allow`.
  * **Not a custodian fit.** A custodian version would need
    `org.freedesktop.systemd1.manage-units`, effectively root, plus BPF
    capabilities for `apply-cgroup`. That is far worse than what it would
    remove.
  * **Not the only route out anyway.** `2026-08-29 005` measured
    `systemd-run --user` landing in `user@<uid>.service/app.slice`, which is
    also outside a firewalled system scope. That measurement was about file
    access; that it also escapes the network filter is inferred. How a
    firewalled activity leaves its firewall is worth an issue of its own.
* **yt-dlp and the media cache.** Downloading needs a network, so the custodian
  is out. But one quick fix turned up: neither `download.rs` nor `playlist.rs`
  passes `--ignore-config` or `--no-plugin-dirs`. So `~/.config/yt-dlp/config`
  and `~/.config/yt-dlp/plugins/`, both writable by any activity, steer every
  download and every playlist resolution `shepherdd` trusts; for example, they
  could add entries to an approved playlist. (`--no-plugin-dirs` is recent;
  check the installed version.) The cache is activity-writable on purpose,
  because `shepherd-media` is an activity (`2026-08-30 001`). If cache integrity
  matters, the answer is a `shepherd-media` uid that owns the cache and runs
  yt-dlp, with read-only access for the player.
* **Input bridges (`input`) and brightness (`video`).** These are device nodes
  and sysfs, blocked by the custodian's `PrivateDevices` / `ProtectKernelTunables`.
  The bridges are also per-activity helper processes the custodian would have to
  spawn. Input injection is already possible through Wayland's virtual-keyboard
  protocol (`2026-08-23 001`).
* **Audio, display power, suspend.** Session-scoped by nature: PipeWire, sway,
  and logind's `yes` for the active session.
* **RPC dispatch, web-auth logic, time accounting.** Already in the trusted
  cgroup, with their state already behind the custodian.

## Future features that would land in the same place

Anything a parent might set from management that polkit reserves for
administrators follows #194's pattern: a typed request, a polkit rule for
`shepherd-state`, and a "what this widens" comment. Examples:

* timezone and NTP. `timedate1.set-timezone` is `auth_admin_keep` for the
  active session, and should stay out of the kiosk's reach because it moves
  bedtime windows.
* hostname.
* pairing Bluetooth audio devices (see §1).
* scheduled reboots.

## Suggested order

1. yt-dlp: `--ignore-config` and `--no-plugin-dirs`. Tiny and independent.
2. Measure the BLE attack on a device.
3. Browser policy via the custodian. It extends the same protocol #194 does.
4. BLE authority into the custodian, after the measurement and #149's decision.
5. The IPC socket passed as an fd.

Also found in passing: the `bluetooth` group comment in `scripts/lib/install.sh`
is wrong for 26.04, and the firewall escape via `systemd-run --user` has no issue
tracking it.

## Which groups the kiosk user would stop needing

Follow-up prompt: "what groups would the child user no longer need then"

The installed kiosk user today:
`shepherd-kiosk video users input bluetooth shepherd-firewall shepherd-waydroid`.

| group | what it gates on this host | after these proposals |
| --- | --- | --- |
| `bluetooth` | **nothing measurable.** No D-Bus, udev or readable polkit rule names it, and no file under `/dev`, `/run`, `/etc` or `/var/lib` is owned by it. BlueZ admits every uid (§1). | **Not needed** once BLE lives in the custodian. Dropping it alone changes nothing: the D-Bus policy fragment in §1 is what takes BlueZ away from the kiosk user. |
| `netdev` | `settings.modify.system` (Ubuntu's NM rule) | Never added. #194 goes through the custodian instead. |
| `video` | `/sys/class/backlight/*/brightness` (`root:video 0664`). `/dev/dri/card*` is also `video`, but carries a seat ACL. | Stays under these proposals. It could go independently by using logind's `Session.SetBrightness`, which only the session owner may call. That is fewer groups but not less exposure: activities are in the same session. |
| `input` | `/dev/input/event*`, and `/dev/uinput` via shepherd's udev rule | Stays. The bridges need device nodes. |
| `shepherd-firewall` | `pkexec shepherd-firewall-helper` as root | Stays. The custodian can't host it (see "Checked, and not the custodian's"). |
| `shepherd-waydroid` | `pkexec shepherd-waydroid-helper` as root. Branch `u/albert/2/android-activity` (#2), installed on this device but not on `main`. | Stays, for the same reason: the helper runs `waydroid shell` and `systemctl start`, i.e. child processes as root. |
| `users` | stock | — |

So the honest count is **one group, `bluetooth`, and it is already inert**.
The groups that matter stay, because what they gate either spawns root helpers
or opens device nodes, and the custodian does neither.

### The problem with the two `pkexec` groups is who they admit

Both polkit rules are `subject.isInGroup(…)` and nothing else, and every
activity is in the group.

* `shepherd-firewall-helper` checks `--uid == PKEXEC_UID`.
* `shepherd-waydroid-helper` (on the #2 branch) checks nothing about its caller.
  Its subcommands include `unlock`, which clears Android Lock Task, plus `pin`
  and `force-stop`. An activity with a shell on the host can run any of them as
  root. Android apps themselves can't reach host `pkexec`.

Removing the group isn't the fix: polkit has to name *someone*, and any uid it
names includes the activities. What would narrow it is the helper checking its
caller's cgroup. `pkexec` `exec`s in place, so the helper's parent is the
process that invoked it: `pidfd_open(getppid())`, confirm the parent hasn't
changed, and compare its cgroup id with the kiosk session scope. That is the
comparison `shepherd-ipc`'s `PeerPolicy` and the custodian already make.
Activities are in scopes of their own, so they would be refused. *Not built,
not measured.* It belongs in both helpers, and is worth raising on #2 before
that branch merges.

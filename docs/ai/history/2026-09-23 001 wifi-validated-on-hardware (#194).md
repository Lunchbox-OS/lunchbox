# Choosing a Wi-Fi network (#194) — validated on hardware, and what that fixed

**Prompts:** "#218 is checked out. rebase atop the current main, then perform
the validation. you have two phones, a BLE dongle, and a wifi dongle. I also
created a test network for you", then, after the findings, "fix 1–5 on this
branch, then push", then "rewrite the PR as reviewable changes, then retest
the whole thing".

**Issue:** <https://github.com/aarmea/lunchbox/issues/194> · **PR:** #218

Follows `2026-09-22 001 wifi-configuration-implementation (#194).md`, whose
"NOT verified" section is what this session was asked to finish: the
custodian's write path on an installed device, and the companion's Wi-Fi
screen over BLE.

## The bench

Unlike the one that built the feature, this one had what that note asked for:
Ethernet carrying SSH (`enp1s0`), a spare USB Wi-Fi dongle (`wlx00c0caa65502`,
an RTL8812AU on `rtw88_8812au`), the Realtek BLE adapter (`8C:68:8B:41:02:DC`,
LESC-capable, so Numeric Comparison works first try), the Pixel, and a test
network, `TP-Link_Guest_50A6_5G`, in WPA2/WPA3 transition mode.

### Running the custodian on a dev box

Worth writing down, because it is not obvious and every step matters:

1. `sudo ./scripts/lunchbox install state --user <you> --debug`, then
   `install policy` with a config that pins the BLE adapter.
2. **Remove `50-lunchbox-session-guard.rules` for the duration**, then reload
   polkit. Without it the watchdog fires when the dev stack stops and is
   refused, which is its documented inert mode. With it, stopping the stack
   would end the developer's graphical login.
3. The custodian trusts exactly one cgroup: the user's *graphical logind
   session*. An SSH shell is not in it. If the user has a desktop login (here
   GNOME on seat0, `session-15.scope`), move the shell into it before booting
   the headless stack, so that sway and `lunchboxd` inherit it:
   `echo $$ | sudo tee /sys/fs/cgroup/user.slice/user-1000.slice/session-15.scope/cgroup.procs`.
   This also makes polkit treat `lunchboxd` as an active local session, as
   on a device.
4. `headless.sh` refuses to boot without `--no-state-custodian`, so a
   throwaway local edit strips it from the derived sway config after the
   assertion. It was never committed.
5. The enrolment code lives with the custodian now:
   `/var/lib/lunchboxd/admin/web-auth.toml`.
6. Undo it all afterwards: `uninstall state`, remove `/var/lib/lunchboxd`,
   the `lunchbox-state` user, the enabled socket symlink, and the placeholder
   `~/.config/lunchbox/config.toml` the installer leaves behind. Then restore
   `/etc/netplan` from a backup, because every forget rewrites it.

## What the first pass found

Over HTTP and over BLE from the phone, against the real custodian:

1. **A new network could be saved but never joined.** `AddAndActivateConnection2`
   needs `network-control` as well as `settings.modify.system`, and the rule
   granted only the second. NetworkManager's audit line:
   `op="connection-add-activate" uid=994 result="fail" reason="Not authorized
   to control networking."` Every earlier measurement was taken from a
   logged-in shell, and polkit grants `network-control` to any active local
   session. The custodian has no session.
2. **That refusal reached the API as HTTP 500**, "IO error: asking
   NetworkManager to join the network". The earlier 403 mapping covered only
   `lunchboxd`'s own calls.
3. **A refused join stayed `connecting` for good**, and its leftover watcher
   timed out 90 s later and wrote "gave up after 90 seconds" over the *next*
   join, two seconds into it. Device signals are per device, not per join.
4. **The last failure never went away.** The phone showed "No network called X
   answered" directly above X marked Connected. Connect from the saved list
   showed no progress at all, because `lunchboxd` activated the profile itself
   and only the custodian reports join state.
5. **Re-saving a known network with "join now" wrote the new password to disk
   before trying it**, so a typo replaced a password that worked.

## What fixing them found

Each of these turned up while checking the fixes above on hardware:

* **`find_by_uuid` returned the UUID where its callers expected the SSID.**
  It was only visible in the log (`forgot a Wi-Fi network ssid=194a2246-…`)
  until Connect went through the custodian. Then the join state would have
  read "Connecting to 194a2246-…".
* **An accepted join could still report HTTP 500**, "Resource temporarily
  unavailable". After activating, the custodian called `GetSettings` on the
  new profile only to learn its UUID. `busctl monitor` showed NetworkManager
  holding that reply for **exactly 6.0 s** whenever it was busy (reproducibly,
  right after reloading the Wi-Fi driver), and `lunchboxd` gives each custodian
  call 5. The PR's original code does the same; an A/B against its binary
  showed an identical 5.26 s failure. The custodian now chooses the UUID
  itself.
* **"Active" meant "being tried".** Both lists took it from
  `ActiveAccessPoint` or the active connections, which NetworkManager sets
  when association *starts*. The first cut of fix 4 therefore hid a
  wrong-password failure four seconds later, while NetworkManager was retrying
  a saved profile whose password was also wrong. Active now means activated.

## Where the fixes went

They were first pushed as nine commits on top of the implementation. The PR
was then rewritten so that each fix lives in the commit that introduced the
code it corrects: a reviewer reads each piece once, already right, rather than
a design and then its corrections. The final tree was byte-identical before
and after the rewrite, apart from this note and the web fix below.

| fix | now part of |
| --- | --- |
| 1: grant and check `network-control` | *Let the state custodian save and join a Wi-Fi network* |
| 2: `Refused` → 403 | the protocol commit (the kind), the custodian (sending it), *Send Wi-Fi changes from lunchboxd to the custodian* (mapping it) |
| 3: join-state lifecycle, generations | the custodian commit |
| SSID, not UUID, from `find_by_uuid` | the custodian commit |
| 4a: Connect through the custodian | the lunchboxd commit |
| "active" means activated | *Read the scan list and saved networks from NetworkManager* |
| 4b: settled state checked against the radio | its own commit, *Stop reporting a join the radio has since contradicted*: a decision, not a correction |
| 5: volatile trial over a saved network | the custodian commit |
| the custodian picks its own UUIDs | the custodian commit |

Two structural changes came with it. The Kotlin generator's struct-parameter
fix now comes *before* the RPC surface, so no commit ships generated Kotlin
that does not compile. And the original "Save and join a network" commit is
split in two: the custodian's side, and `lunchboxd` forwarding to it.

## Verified after the fixes

With the rule exactly as shipped, on the same bench:

* `can_configure: true`, no `wifi_config_unavailable`; NetworkManager logged
  `connection-add-activate … uid=994 result="success"`.
* With a well-formed rule granting only `settings.modify.system`: the join is
  refused as **403**, the join state does not move, save-for-later still
  works, and the custodian's startup check names `network-control` as the
  missing grant.
* No "gave up" appeared past the point where the old watcher would have
  written it.
* Connect goes through the custodian and reports `connecting` →
  `connected`, named by SSID. A stale id is still a 404.
* A join right after a driver reload answers in 0.04 s, down from 5.2 s.

The real dongle could not exercise fix 5's success path (see below), so it ran
on the `mac80211_hwsim` bed from
`crates/lunchbox-host-linux/tests/wifi_networkmanager.rs`, with one station and
one WPA2 AP. The dongle was set unmanaged for the duration so the custodian
picked the virtual station. **The AP needs a DHCP server** or every join ends
`no_address`, which is at least the right classification:
`sudo ip addr add 10.218.0.1/24 dev wlan1` and `dnsmasq --port=0
--interface=wlan1 --bind-interfaces --dhcp-range=…`. The scenario was a
router whose password changed:

1. Saved with `oldpassword1`. NetworkManager's autoconnect sits in "need
   authentication".
2. Join with a wrong password: `wrong_password` in about 12 s, and it
   **stays** while NetworkManager retries the old profile. Saved profile and
   psk untouched, no trial left behind.
3. Join with `correcthorse`: `connected`, the trial persisted, the old
   profile retired (`retired the profile it replaces`), one profile left.
   Disconnecting from the host then turns a stale `connected` into `idle`.

The phone was not re-run at this point, since the custodian reinstall had
reset the admin record and it meant pairing again. The retest below did.

## The retest, after the rewrite

Every one of the fourteen commits builds and passes `cargo fmt --check`,
`clippy -D warnings` and `cargo test --workspace --no-fail-fast` on its own,
with only the known failures below. At HEAD: the web UI's CI checks
(`check:boundary`, `check:coverage`, 285 tests, typecheck), the Android build
and unit tests, and `config validate`.

Then the whole feature again from a clean install:

* **Over HTTP, real dongle, rule as shipped:** `can_configure: true`, no
  `wifi_config_unavailable`; NetworkManager accepted the custodian's join
  under the UUID the custodian chose. The dongle then failed to associate
  four times out of four, as it does, and each failed join left nothing
  saved and nothing in `/etc/netplan`.
* **Rule granting only the write:** 403, join state unmoved, and the startup
  check names `network-control`.
* **From the Pixel over BLE, against the virtual AP.** A fresh Numeric
  Comparison pairing and claim, then:
  * the in-range list, and a password sheet that opens empty;
  * a wrong password, with "The password for … was refused" shown;
  * the right password, with "Connected", the Saved and Connected tags and
    the saved list;
  * the router-password-changed case: the outdated profile survived a wrong
    password, and the right one replaced it, leaving one profile;
  * Connect from the saved list, showing "Connecting…" and then
    "Connected";
  * Forget;
  * with the DHCP server stopped, "accepted the password but never gave the
    device an address";
  * "Other network…" with a hidden SSID, which reached NetworkManager with
    `hidden=yes` and ended "No network called … answered".
* **The web UI in Firefox**, driven over Marionette: Save for later, then
  Connect now through its "This page may stop responding" confirmation to
  "Connected", with the device's new address under *Web interface*.

### What the retest found

* **The web panel's own "Connecting to X…" notice outlived the join.** After
  "Connect anyway" it stayed on screen under the daemon's "Connected to X."
  until somebody closed it; the phone's equivalent is a transient status
  line and clears itself. A join's progress is now left to the daemon's join
  state, and the panel keeps its own notices for save-for-later, forget and
  scan. Folded into the web UI commit, with a test that fails without it.
* **`bluetoothd` segfaulted** right after the host's bond with the phone was
  removed (`kernel: bluetoothd[1309]: segfault at 20 … in libc.so.6`), and
  systemd restarted it. `lunchboxd` did not re-register its advertisement
  with the new daemon (`ActiveInstances` 0 while its log still said
  advertising had started), so no phone could find the device until the
  stack was restarted. The crash is BlueZ's; the missing re-registration is
  `lunchbox-ble`'s, predates this PR, and wants its own issue.
* Two things that are the test bench, not the product, and cost time:
  * Firefox accepts the dev stack's self-signed certificate only for the
    life of the Marionette session, so once the driver script exits the
    page shows "AxiosError: Network Error" on its next poll. A new session
    loads it fine.
  * Several web tests time out at 5 s when the machine is loaded (load
    average 16 on 8 cores, with Firefox, the stack and the suite at once).
    A different set failed on each such run, including unrelated
    file-picker tests; the suite passed 285/285 twice once the machine was
    idle.

## Things worth knowing

* **The RTL8812AU dongle is unreliable here.** It associates about one time in
  five, and plain `nmcli` fails the same way (`ssid-not-found`, no auth frame
  sent). NetworkManager's autoconnect retries usually get through where a
  one-shot activation does not, which is why a volatile join (one attempt)
  fails more often than a saved profile does. A better dongle would make
  future join tests much cheaper.
* **Netplan on forget:** on every forget this session (about eight, through
  Lunchbox, `nmcli` and the phone), both base files were rewritten
  without their comments, but `00-installer-config.yaml` was **never
  deleted** and its settings were unchanged. The earlier note's deletion did
  not reproduce here. The hazard is milder than recorded, but real.
  *(Later in the same PR: the deletion reproduces as soon as a later file
  mentions an interface the installer's file defines, and forgetting no
  longer goes through NetworkManager's delete at all. See
  `2026-09-24 003 netplan-safe-forget (#194).md`.)*
* **Not fixed, deliberately out of this change:**
  * Several messages and rule headers still say
    `/etc/polkit-1/rules.d/…` where the installer puts
    `/usr/share/polkit-1/rules.d/…`. This is shared with the session-guard
    and firewall rules from earlier issues, so it wants its own change.
  * "Save for later" still autoconnects at once on an idle radio.
  * Forget has no confirmation, even for the network the device is on.
  * The hidden-network failure sentence tells someone who *did* add it by
    hand to add it by hand.
* The `lunchbox-http` upload failures, 9 in `--test files` and 6 in
  `--test files_on_disk`, reproduce on unmodified `main`; they are not this
  branch's. Plain `cargo test --workspace` stops at the first failing binary,
  so it shows only one of the two: use `--no-fail-fast`.
* `lunchbox-host-linux`'s `retroarch_spawn_materializes_config_and_argv` is
  timing-flaky (it sleeps 300 ms and reads a file), about 1 run in 6 on this
  box, and nothing here touches it.

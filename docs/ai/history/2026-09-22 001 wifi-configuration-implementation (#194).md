# Choosing a Wi-Fi network from the management UIs (#194) — the implementation

**Prompt:** "rebase again (making sure to renumber), then do the whole thing"
— following "let's pick up where we left off on #194", which had asked for
hardware measurement first.

**Issue:** <https://github.com/aarmea/lunchbox/issues/194>

Builds on two earlier notes, both on this branch:

* `2026-09-11 006 wifi-configuration-scope (#194).md` — the design.
* `2026-09-21 004 wifi-against-real-networkmanager (#194).md` — the
  measurements, which corrected two of the design's mechanisms and found one it
  had missed entirely.

The maintainer's answers to the scope note's open questions, given at the start
of this work: **the web may join a network immediately, behind a warning**, and
**Enterprise stays out of v1**.

## What shipped

All three of the scope note's slices, in nine commits:

| commit | what |
| --- | --- |
| Describe a wireless network… | the wire types, validation, aggregation |
| Put the wireless backend behind a trait | the `WifiController` seam |
| Offer the six Wi-Fi methods… | the RPC surface, audit rows, codegen |
| Read the scan list and saved networks… | the NetworkManager reader |
| Give the custodian a typed vocabulary… | the custodian protocol |
| Save and join a network… | the custodian's writes, the polkit rule |
| Teach the Kotlin generator… | a codegen bug this feature exposed |
| Join a saved network without the custodian… | two bugs the live bus found |
| Choose a Wi-Fi network from the browser | the web UI |
| Pick a Wi-Fi network from the phone | the companion UI |

## The shape, and the one place it departs from the scope note

Reads run in `lunchboxd`; writes go to the state custodian. That is the scope
note's decision C, and the measurement note proved the argument for it:
`settings.modify.system` alone was enough to read back every saved network's
passphrase through `GetSecrets`, with no separate polkit action gating it. Had
the kiosk user been put in `netdev` — one line in the installer — every
activity a child can start would have been able to read the house WiFi key.

**The departure is which operations count as writes.** The scope note's rule
was "anything that changes the device goes through the custodian", and the
first implementation followed it. That was wrong, and the live daemon is what
showed it: activating a profile the device *already has* is gated on
`network-control`, which polkit grants to an active local session outright.
Routing it through the custodian meant a device whose polkit rules file was
missing could not get back onto a network it already knew — while the
`wifi_config_unavailable` diagnostic, and both UIs, told the parent in as many
words that it still could.

So the split follows the measured permission table rather than the tidier rule:

| operation | polkit action | where it runs |
| --- | --- | --- |
| scan | `wifi.scan` — yes | `lunchboxd` |
| list saved | none (property reads) | `lunchboxd` |
| activate a saved profile | `network-control` — yes | `lunchboxd` |
| write a profile | `settings.modify.system` — `auth_admin_keep` | **custodian** |
| delete a profile | `settings.modify.system` | **custodian** |

The other departure is smaller: the custodian owns a join **end to end** —
start, watch, persist on success — rather than starting it and handing the
watching back to `lunchboxd`. The requests stay short either way, because
`save` returns when NetworkManager *accepts* the activation. Splitting it would
have spread one stateful transaction across two processes, on behalf of a
client that could crash between the halves and leave a volatile profile nobody
persists or removes.

## What the polkit rule really grants

`dist/polkit/50-lunchbox-network.rules` says this at length, because the next
person to read it deserves the whole truth: polkit cannot narrow
`settings.modify.system` to wireless profiles, so the rule lets
`lunchbox-state` change **any** NetworkManager setting on the machine, and —
per the measurement — read back every stored network secret.

What makes that acceptable is the socket, not the rule. The protocol's wireless
requests are a typed `WifiJoinRequest` and two UUIDs; the custodian **never
accepts a settings dictionary**, so there is no way to ask it to write a
setting it was not written to write. Nothing in Lunchbox calls `GetSecrets` at
all, because no management UI ever shows a saved password.

Two drift tests hold that in place: one asserts the rule names the action and
the custodian's user, and one asserts it names *no other* user and uses no
`isInGroup` — because a group grant admits every member, and on this device
that means every activity.

## Three bugs only the real stack could find

The unit tests passed throughout. Each of these was caught by driving the live
daemon over HTTP on the headless session.

1. **`specific_object` is a D-Bus object path, not a string.** Passing `&str`
   compiles and fails at call time with `Type of message, "(oos)", does not
   match expected type "(ooo)"` — naming neither argument nor method. It was in
   both `ActivateConnection` and `AddAndActivateConnection2`.
2. **The Kotlin generator cannot encode a struct parameter.** `wifi_save` is
   the first RPC method to take one, so the path had no coverage; the generator
   emitted `JsonPrimitive(request)`, which fails with `Cannot access
   'constructor(): JsonPrimitive': it is protected`. Neither the Rust suite nor
   the codegen drift test can see this — only building the Android app does.
3. **A polkit refusal arrived as HTTP 500.** It is the one failure on that path
   somebody can act on, and it is what a developer sees routinely, because a
   daemon started from an SSH shell is not an active local session.

## Where the copy earns its keep

Two failure messages are the reason the reason-mapping exists at all:

* *"The password for X was refused"* — NetworkManager reason 7.
* *"X accepted the password but never gave this device an address. Check the
  router's DHCP."* — reason 5.

Both are "it did not work". Only one is about the password, and a parent shown
the first when the second happened will retype a correct password until they
give up. The measurement note is what made the distinction available; without
it, both would have been "could not connect".

An unsupported network — Enterprise, WEP — is **shown** in the list and marked,
rather than filtered out, because a network missing from a list reads as a
device that cannot see it.

## Verified

**Against the real stack**, through the headless session's HTTP API on the dev
box (`--no-state-custodian`, so `can_configure` is false and the write path
reports its refusal):

* `wifi_networks` returned the live scan; `wifi_saved_networks` returned both
  of this device's profiles for one printer's SSID, correctly distinguished as
  WPA2 and WPA3 — the exact trap the scope note warned about.
* Validation refused a five-character PSK (400), a U+2019 curly quote (400) and
  an Enterprise request (400), each with its own sentence.
* `wifi_save` and `wifi_forget` refused with 403 for want of a custodian;
  `wifi_connect` with a stale id answered 404, and with a real id reached
  NetworkManager and was refused by polkit — reported as 403.
* The web panel was rendered in Firefox and screenshotted: the "cannot save a
  network" warning, the in-range list with its Connected chip, and the saved
  list showing both same-named profiles apart by security.

**Against real beacons**, on a `mac80211_hwsim` bed of four virtual radios
(`crates/lunchbox-host-linux/tests/wifi_networkmanager.rs`, `#[ignore]`d, with
the recipe in its module docs): WPA2-PSK, WPA3-SAE and a genuinely open network
each classified correctly — the last being the one that needs the absent
`PRIVACY` bit — and the scan age came back as 16 seconds where the live
adapter's genuinely stale scan read 12730.

`LUNCHBOX_WIFI_INTERFACE` exists so that test can run at all: the dev box's
only real radio carries the SSH session, so the integration tests point at a
virtual one instead.

**Not verified:** the write path on an installed device, which is the only
place the polkit rule and the custodian's NetworkManager calls actually
execute. That needs `lunchbox install state` on a real kiosk, the way #172 was
checked. Everything up to the socket is exercised; what happens on the far side
of it has unit tests and no hardware run.

## Left undone, deliberately

* **Enterprise and WEP**, per the maintainer's answer. Both appear in the scan
  marked unsupported, with admin mode (#154) as the way in.
* **Toggling the radio.** `radio_enabled` is reported, never set.
* **Manual IP configuration and captive portals**, per the issue.
* **Choosing between several adapters.** The first managed one wins;
  `LUNCHBOX_WIFI_INTERFACE` is the escape hatch and is not a UI.

## Worth a follow-up issue

**netplan pins every saved profile to the interface it was created on.** This
is not something Lunchbox does — netplan normalises every profile that way,
including the two GNOME wrote on this device long before any of this. The
consequence is a real support case: replace a USB wifi dongle and every saved
network silently stops matching, on a device that is then offline — which is
exactly the state #194 exists to rescue a parent from.

**A forget rewrites all of `/etc/netplan`.** Measured during the investigation:
deleting one profile stripped a comment from `01-network-manager-all.yaml` and
removed `00-installer-config.yaml` outright. `WifiCustodian::forget` documents
it; nothing in this change can prevent it. Before a household relies on the
button, it wants either a check that `/etc/netplan` is otherwise untouched or a
recorded decision that this is acceptable.

*Fixed later in the same PR: forgetting no longer goes through NetworkManager's
delete. See `2026-09-24 003 netplan-safe-forget (#194).md`.*

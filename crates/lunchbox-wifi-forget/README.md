# lunchbox-wifi-forget

A root oneshot that forgets one saved Wi-Fi network without letting netplan
rewrite `/etc/netplan` (issue #194).

## Why this exists

On Ubuntu, NetworkManager stores each profile as a netplan definition,
`NM-<uuid>`, in its own `/etc/netplan/90-NM-<uuid>.yaml`. Saving a profile
writes only that file. Deleting one does not: NetworkManager calls
`netplan_delete_connection`, which parses the whole hierarchy and writes every
file back out from what it parsed (`netplan_state_update_yaml_hierarchy`,
netplan 1.2). So one forget:

* strips every comment from every file in `/etc/netplan`, and
* unlinks any file whose definitions a later file also mentions, because netplan
  credits them to the later one. That is how a forget once removed the
  installer's `00-installer-config.yaml`.

Measured, and read in the source, in
`docs/ai/history/2026-09-21 004 wifi-against-real-networkmanager (#194).md`
and `2026-09-24 003`.

No NetworkManager call avoids it. The state custodian cannot do the edit
itself either: it holds no capabilities, `/etc/netplan` is root's, and
telling NetworkManager afterwards needs `LoadConnections`, which
NetworkManager's D-Bus policy admits from root alone.

## What it does

`lunchbox-wifi-forget <uuid>`, as `lunchbox-wifi-forget@<uuid>.service`:

1. Takes `/run/lock/lunchbox-wifi-forget.lock`, so two forgets never edit the
   same file at once.
2. Reads every `*.yaml` under `/lib/netplan`, `/etc/netplan` and
   `/run/netplan`. For each file that defines `NM-<uuid>`, it plans an edit that
   removes that one key with [`yaml-edit`], which keeps every other byte:
   comments, quoting, indentation, flow style. If the definition was the only
   Wi-Fi network in the file, the now-empty `wifis:` goes too. The profile's
   own `90-NM-<uuid>.yaml` is unlinked once nothing but `version` is left,
   which is what NetworkManager would have done with it.
3. Checks every edit before writing anything (see below).
4. Writes each file atomically (temporary file, `fsync`, `rename`), keeping
   its owner and mode. These files hold passphrases in plain text.
5. Runs `/usr/libexec/netplan/configure --networkmanager-only`, the exact
   command NetworkManager runs after its own delete. If that fails, every file
   is put back.
6. Runs `nmcli connection load` on the profile's generated file under
   `/run/NetworkManager/system-connections`, which is now gone. NetworkManager
   drops the profile, and disconnects if it was in use, as `Delete` would.

A profile that is not stored in netplan (a keyfile, or one that only lived in
memory) never reaches this. The custodian deletes it the ordinary way, because
there the ordinary way touches nothing else.

## How an edit is checked

This runs as root, and `yaml-edit` is young, so no edit is written until three
checks pass:

1. **Meaning.** The old and new texts are parsed by [`yaml-rust2`], an
   independent parser, and the new one must equal the old one minus the
   definition. This also rejects duplicate keys, and aliases left pointing at
   the removed anchor.
2. **Shape.** The new text must be the old text with one contiguous piece cut
   out. A reformat that keeps the meaning, which is what netplan's own delete
   does, fails here.
3. **Comments.** The cut may contain a `#` only inside the removed entry. A
   comment just above the definition stays, orphaned rather than guessed at.

If any file fails any check, or cannot be parsed, or is a symlink, or is
packaged configuration under `/lib/netplan`, or defines the id as something
other than a Wi-Fi network, **nothing is written anywhere**. The unit fails
and the journal says which file and why.

## Trust boundary

* The custodian starts the unit over systemd's D-Bus API.
  `dist/polkit/50-lunchbox-network.rules` grants `lunchbox-state` the
  `start` verb on `lunchbox-wifi-forget@<uuid>.service`, with the instance
  anchored to a canonical UUID, and nothing else in systemd.
* The only input is the instance name, validated again here as 36 characters
  of lowercase hex with hyphens in NetworkManager's positions.
* That is strictly less than the custodian already holds:
  `settings.modify.system` lets it delete any profile, and this deletes one
  Wi-Fi profile by UUID.
* The unit is sandboxed (`ProtectSystem=strict` with only `/etc/netplan` and
  `/run` writable, no network, a capability set of `CAP_CHOWN`, `CAP_FOWNER`
  and `CAP_DAC_OVERRIDE`). Its dependencies are the two YAML crates and
  `std`. It runs two programs, both by absolute path.

[`yaml-edit`]: https://crates.io/crates/yaml-edit
[`yaml-rust2`]: https://crates.io/crates/yaml-rust2

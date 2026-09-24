# Forgetting a Wi-Fi network without rewriting /etc/netplan (#194)

**Prompt:** "I think the netplan clobbering deserves a fix right here in this
PR. fwiw, we already know how to not clobber YAML comments -- see the frontend
config editor", on PR #218 after its hardware retest. The "clobbering" is the
finding first recorded in `2026-09-21 004`, §3: a forget rewrote every file in
`/etc/netplan`, stripped their comments, and once removed the installer's
`00-installer-config.yaml` outright.

## Where the damage happens

Not in Lunchbox. Read in the source of the exact versions installed (netplan
1.2, and Ubuntu's `applied/1.54.3-2ubuntu3` of NetworkManager, whose netplan
integration is an Ubuntu patch that upstream does not carry):

* **Saving a profile is harmless.** The keyfile writer hands the profile to
  `netplan_netdef_write_yaml`, which writes `/etc/netplan/90-NM-<uuid>.yaml`
  and nothing else.
* **Deleting one is not.** `nms-keyfile-plugin.c` calls
  `netplan_delete_connection(id, NULL)` then `generate_netplan`. The first
  parses the whole hierarchy (`/lib`, `/etc`, `/run`), applies a `NULL` patch
  to the one definition, and calls `netplan_state_update_yaml_hierarchy`,
  which **truncates and re-emits every file** from its parsed state. It then
  **unlinks every source file left without definitions of its own**, and a
  definition belongs to the last file that mentions it.

The second rule is the deletion from `2026-09-21 004`, reproduced on purpose
this time. With a hand-written `95-lunchbox-hand.yaml` that also mentions
`enp1s0`, one `nmcli con delete` of an unrelated Wi-Fi profile removed
`00-installer-config.yaml` and folded its settings, without the comment, into
the hand-written file. The retest's "did not reproduce" was simply a device
where no later file mentioned `enp1s0`.

No D-Bus call avoids it. Every to-disk profile on this Ubuntu goes through
netplan, and `Delete` is the only removal NetworkManager offers.

## The design

The custodian cannot do the edit itself. It holds no capabilities, runs with
`ProtectSystem=strict` and `NoNewPrivileges` (so no `pkexec`, the firewall
helper's route), and `/etc/netplan` is root's. Telling NetworkManager
afterwards needs `LoadConnections` or `ReloadConnections`, which
NetworkManager's D-Bus policy (`org.freedesktop.NetworkManager.conf`) denies
to everyone but root.

So there is a root oneshot, `lunchbox-wifi-forget@<uuid>.service`, which the
custodian starts over systemd's D-Bus API. A second clause in
`50-lunchbox-network.rules` grants `lunchbox-state` the `start` verb on that
template only, with the instance anchored to a canonical UUID. That is strictly
less than the first clause, which already lets this uid delete any profile.

The sequence, each step first tried by hand on this machine:

1. Remove the definition from the YAML, and unlink the profile's own file once
   it is empty.
2. `/usr/libexec/netplan/configure --networkmanager-only`, the exact command
   NetworkManager runs after its own delete. Traced: it writes only under
   `/run`, and removes just that profile's generated `.nmconnection`. Every
   other generated file kept its timestamp.
3. `nmcli connection load <that generated file>`, now missing. NetworkManager
   drops the profile, and when it was the active one, disconnects, exactly as
   `Delete` does.

Asked whether the helper should edit files other than the profile's own when
they mention it (rare: a `netplan set` override, a hand edit), or refuse them,
the user chose **edit, format-preserving**, the config editor's `toml_edit`
approach applied to YAML.

## The editor, and why its output is checked

`yaml-edit` 0.3 (pure Rust, a lossless `rowan` tree) removes one key and keeps
every other byte. Tried first on the shapes that matter: a neighbouring
definition with comments, the definition as the only Wi-Fi network, a quoted
key, flow style, and a file with a comment before `network:` (which needs
`YamlFile` rather than `Document`: the subiquity file starts that way). Every
one came out as a clean excision. A comment just above the removed definition
stays behind, orphaned. That is the conservative choice: nothing a person wrote
is deleted on a guess.

`yamlpatch` (from zizmor) was the other candidate. It brings tree-sitter's C
runtime and generated parser into a root process, which is why it lost.

`yaml-edit` is young and, by design, recovers from syntax errors rather than
rejecting them. So an edit is written only if three checks pass:

1. `yaml-rust2`, an independent parser, reads the new text as exactly the old
   meaning minus the definition (and minus `wifis:`, if that emptied it). It
   also rejects duplicate keys, and fails on an alias left dangling.
2. The new text is the old text with one contiguous piece cut out, so a
   reformat that keeps the meaning, which is what netplan does, fails.
3. That piece holds a `#` only inside the removed entry.

The tests feed `verify` deliberately wrong edits of each kind, because the
editor passing every other test says nothing about whether the checks can
fail. Any refusal, anywhere, means nothing is written anywhere.

## What the measurements turned up along the way

* **polkit will not take details from the custodian.** The startup check would
  have asked about the unit grant with `unit` and `verb` details, since the rule
  tests both. polkit 127: "Only trusted callers (e.g. uid 0 or an action owner)
  can use CheckAuthorization() and pass details". Without them the rule cannot
  match, so the check would report "refused" on every device. There is no
  startup check for this grant. It shares a file with the two that are
  checked.
* **systemd's refusal is `org.freedesktop.DBus.Error.InteractiveAuthorizationRequired`**,
  and through a zbus proxy it arrives as a plain `MethodError`, not as zbus's
  typed `fdo` error. The first version matched the typed one, so a missing
  grant came back as a 500 reading only "starting lunchbox-wifi-forget@…". It
  is a 403 now, matched by name like NetworkManager's refusals.
* **`File::lock`** (std since 1.89) serialises forgets without a new
  dependency. Two profiles are two units, so systemd would run them at once,
  and both may need to edit the same hand-written file.

## Verified

On an installed custodian (installer as shipped, then `uninstall state`, which
also removed the helper and unit), against the `mac80211_hwsim` AP, comparing
checksums of both base files after every step:

| case | result |
| --- | --- |
| API forget of the active network | profile gone, device disconnected, base files byte-identical |
| web UI's Forget button, active network | "Network forgotten.", same |
| join with a new password over a saved one | old profile retired through the unit, one profile left, base files identical |
| profile also overridden in a commented hand-written file | only that key removed; header and ethernet comments kept |
| malformed file under `/run/netplan` mentioning the profile | 500 naming the journal; `/etc/netplan` unchanged; profile still saved |
| rules file without the unit clause | 403 "not allowed to change Wi-Fi settings" |
| same situation through `nmcli con delete` (control) | `00-installer-config.yaml` unlinked, comments gone |

Every commit builds and passes fmt, clippy `-D warnings` and the workspace
tests, apart from the known local upload failures.

## Things worth knowing

* **`/tmp` is a 5.6 GB tmpfs**, and another session's scratchpad held 3.9 GB of
  it. Cloning NetworkManager into this session's scratchpad filled it. The
  symptom was every Bash call failing with exit 1 and no output, even `echo`,
  because the tool's output capture lives there too. Large reference checkouts
  went to `~/.cache/lunchbox-ref` instead, sparse. The other session's files
  were left alone.
* **A new binary crate belongs in `default-members` too**, not only
  `members`. `lunchbox package deb` runs a plain `cargo build --release`,
  which builds default members only, and the comment above that list says
  every crate but the codegen and wasm ones belongs there. Every local check
  here used `--workspace`, so the omission surfaced only as CI's `.deb` smoke
  build failing with "lunchbox-wifi-forget not found at target/release".
* `lunchbox package deb` stages under `$TMPDIR`. On this machine, with the
  tmpfs already mostly taken, it filled `/tmp` and took the tool's output
  capture down with it. `TMPDIR=~/.cache/...` avoids that.
* `pkill -f <pattern>` inside a Bash call matches that call's own shell,
  whose command line contains the pattern, and kills it partway.
* The retest's note that the deletion "did not reproduce" was right about that
  machine and wrong as a conclusion. It depends on whether some later file
  mentions an interface the installer's file defines.

## Still open

* netplan pins every saved profile to the interface it was created on, so
  replacing a USB dongle orphans every saved network. Unchanged by this, and
  still worth its own issue.

# `conffiles` defends vendor files shipped into admin directories (issue #177)

**Prompt:** `fix #177`

**Issue:** <https://git.armeafamily.com/albert/shepherd-launcher/issues/177>

## What the issue said

Four files were declared as dpkg `conffiles` in `scripts/lib/package.sh`:

```
/etc/sway/shepherd.conf
/etc/udev/rules.d/71-shepherd-uinput.rules
/etc/polkit-1/rules.d/50-shepherd-firewall.rules
/etc/systemd/system/bluetooth.service.d/10-shepherd-bluetooth-experimental.conf
```

`conffiles` means "the admin owns this; preserve their edits and prompt on
upgrade". None of the four was that, and one misbehaved:

1. **The bluetoothd drop-in was a conffile the postinst rewrote.** dpkg
   checksums a conffile at unpack; the postinst then `sed`ed the build host's
   `bluetoothd` path out of it. Every *later* upgrade therefore saw a
   locally-modified conffile, on every device, without anyone having edited
   anything — and either prompted or (under `--force-confold` / unattended
   upgrades) silently kept the stale copy.
2. **`shepherd.conf` was a conffile while `docs/INSTALL.md` said the opposite.**
   The file is generated with rewrites `install_sway_config` verifies and `die`s
   on (#144, #157, #172). Those checks run when it is written and nowhere else,
   so a preserved admin copy carried unverified content across upgrades forever
   — including an exec line that predates #172's fix, on exactly the devices
   whose operator was hands-on enough to have edited it.
3. **The udev and polkit rules were vendor files in admin directories.** The
   tell: the polkit *action* already shipped to `/usr/share/polkit-1/actions/`
   while the *rule* went to `/etc`, from the same install step.
4. The state custodian's units were already (correctly) not conffiles, and
   nothing recorded why.

## What was done

### 1. The drop-in is rendered by the postinst, not shipped

`install_bluetooth_dropin` now branches on `DESTDIR`: a real install renders the
drop-in as before, packaging stages the *template* (with `@BLUETOOTHD@` intact)
to `/usr/share/shepherd/systemd/`. The generated postinst renders it from there,
picking the daemon path out of `systemctl cat bluetooth.service`.

Rendering rather than shipping-then-`sed`ing is what removes the defect: dpkg
never holds a checksum of a file that is about to change, so there is nothing to
prompt about. It also means an `apt upgrade` re-reads the host's `bluetoothd`
every time, instead of possibly keeping a stale `ExecStart`.

`BLUETOOTHD_DEFAULT_PATH` survives, but its reason changed: it used to cover
"staging on a build host", which no longer renders anything, and now covers
"the unit is there but its `ExecStart` will not parse".

### 2 & 3. The rules moved to their vendor directories, and every conffile went

- `UDEV_RULES_DIR`: `/etc/udev/rules.d` → `/usr/lib/udev/rules.d`
- `POLKIT_RULES_DIR`: `/etc/polkit-1/rules.d` → `/usr/share/polkit-1/rules.d`

Both subsystems read the vendor directory *and* `/etc`, with `/etc` winning — so
an admin override still works exactly as before; shepherd's own copy simply
stopped squatting in the override location.

`shepherd.conf` stays where it is and is now replaced outright on upgrade, which
is what `docs/INSTALL.md` had always promised.

The package now declares **no** `conffiles` at all. The reasoning for each of
the four, and for the state custodian's units staying out of the list, is
written where the file used to be generated in `_package_write_control`.

### The upgrade path (the part that needed care)

dpkg does **not** remove a conffile just because a new version stopped shipping
it: it keeps the file and remembers it as `obsolete`. Left alone, every upgraded
device would keep an older udev and polkit rule in `/etc`, still winning over
the copies that replaced them. So the generated `preinst`/`postinst`/`postrm`
now call `dpkg-maintscript-helper rm_conffile` for those two paths, with
`Pre-Depends: dpkg (>= 1.15.7.2)` as policy requires. `PACKAGE_LAST_CONFFILE_VERSION`
(`0.4.1`) is the `prior-version` argument.

The drop-in is deliberately *not* retired that way. Its path did not move — the
postinst writes it — so `rm_conffile` would rename every device's copy to
`.dpkg-bak` a moment before the postinst wrote a fresh one over the top. dpkg
keeps an obsolete conffile record for it instead, which costs one stale line in
`dpkg-query -W -f='${Conffiles}'` and buys the file being cleaned up on purge.

From-source installs get the same treatment through `remove_superseded_copy`,
called by `install_udev` and `install_firewall` on real (non-`DESTDIR`) installs;
the `uninstall_*` functions remove both the new and the legacy path.

## How it was verified

The whole thing was run for real on the dev box (Ubuntu 26.04, dpkg 1.23.7):
its from-source install was removed, the released packaging was installed, the
defect reproduced, the fix installed over it, and the box put back. Everything
below is from that run unless it says otherwise.

### Reproducing the defect

Installing the pre-change 0.4.1 `.deb` and then upgrading, with the drop-in in
the state a device is in — pointing at *that machine's* `bluetoothd`, which is
what 0.4.1's postinst writes there:

```
Configuration file «/etc/systemd/system/bluetooth.service.d/10-shepherd-bluetooth-experimental.conf»
 ==> Modified (by you or by a script) since installation.
 ==> Package distributor has shipped an updated version.
 ==> Keeping old config file as default.
```

The revised drop-in did not land: dpkg parked it as `.dpkg-dist` and kept the
old one. On an interactive upgrade this is the prompt; unattended it is silent.

**Worth knowing, because it explains why nobody hit this locally:** when the
build host and the target have the same `bluetoothd` path, the postinst's `sed`
is a no-op and the checksum still matches, so the upgrade is quiet. The defect
needs a device whose daemon path differs from the build host's — and it only
turns into a *lost update* when the shipped drop-in also changes between
releases, which any edit to that file's long comment header would do.

### Verifying the fix, on the same box

Baseline: 0.4.1 installed, its drop-in pointing at the device's `bluetoothd`
(so dpkg sees it modified), and `/etc/sway/shepherd.conf` hand-edited the way an
operator might.

1. **0.4.1 → 0.5.0, unattended, no force options.** No prompt and no `==>`
   message. `Removing obsolete conffile /etc/udev/rules.d/71-shepherd-uinput.rules`
   and the polkit one — both admin directories clear, both rules present under
   `/usr`. The operator's sway edit is gone (the file is generated) and
   `shepherd.conf.d/` is untouched. The drop-in is rendered from the staged
   template with this machine's `ExecStart`.
2. **0.5.0 → 0.5.1 with the drop-in template edited** — the same change 0.4.3
   could not deliver above. It lands, `ExecStart` is still correct for the
   machine, and there is no `.dpkg-dist`.
3. **`apt install --reinstall`, no `--force-confmiss`,** after deleting the sway
   config, both rules and the drop-in behind dpkg's back: all four come back
   (the drop-in via the postinst). The diagnostic in `docs/INSTALL.md` names
   exactly the missing files.
4. **`apt remove`** takes the postinst-written drop-in and its directory away —
   dpkg would not have, since it never unpacked it — and the device's state
   under `/var/lib/shepherdd/state/kiosk/` is untouched. `apt purge` clears the
   rest.
5. **The obsolete conffile record** for the drop-in survives upgrade, reinstall
   and remove, and clears on purge. That is what the comments in `package.sh`
   and `path_owned_by_dpkg` say, now measured rather than assumed. On an
   upgraded box `dpkg-query -S` still answers for that path, so a source
   `uninstall all` leaves it — observed, along with every other packaged file.
6. **The subsystems still read the moved rules.** `/dev/uinput` is
   `root:input 0660` from `/usr/lib/udev/rules.d`; `pkcheck --action-id
   org.shepherd.firewall.apply-process` answers `yes` for a member of
   `shepherd-firewall` from `/usr/share/polkit-1/rules.d`; `systemctl show
   bluetooth.service -p ExecStart` resolves through the drop-in.
7. **The kiosk session came up on the new layout.** After putting the box back
   on a from-source install and restarting the session: `shepherdd starting
   version="0.5.0"`, `Store opened db=/var/lib/shepherdd/state/kiosk/shepherdd.db`,
   `Configuration loaded entry_count=17 source="the state custodian"`, `Per-entry
   firewall enforcement is available`, launcher and HUD connected, HTTP API 200,
   and no sway IPC socket left (the generated config still hardens).

One behaviour change falls out of this and is intended: `apt remove` now deletes
`/etc/sway/shepherd.conf` and the two rules, where before they survived as
conffiles until `purge`. They are shepherd's files, so removing the package
should take them.

### Also checked, off the box

- **Synthetic dpkg-root transitions**, run first to establish what dpkg does for
  each of the three cases. The useful finding: a conffile that becomes an
  ordinary file at the *same* path is replaced silently and the record dropped,
  so `shepherd.conf` needs no helper.
- **The postinst's render block, run verbatim** against a stubbed `systemctl
  cat` shaped like the real thing (vendor unit first, then a drop-in from an
  earlier run): it picks the daemon rather than reading its own `ExecStart`
  back, is idempotent, removes the drop-in and its directory when there is no
  `bluetooth.service`, and leaves no `.new` file.
- **Install/uninstall symmetry** — `install_system` then `uninstall_system`
  under one `DESTDIR`: every staged file removed, the new template included.
- `cargo test --workspace --all-targets`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo fmt --all`, `shellcheck`, and
  `shepherd version check` — all clean.

A CI step in the `package` job now asserts the built `.deb` declares no
`conffiles` and ships no `bluetooth.service.d/` drop-in. Both assertions were
checked against the old package to confirm they discriminate.

## A note for the next person

`PACKAGE_LAST_CONFFILE_VERSION` in `package.sh` is `0.4.1` — the last release
that declared conffiles — and this change ships in 0.5.0. It is the
`prior-version` handed to `rm_conffile`, so it must stay at or above the highest
released version that still had them. Upgrading from anything newer finds
nothing to do, so it does not need touching again unless a conffile is
reintroduced.

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

Everything below was run in a throwaway dpkg root (`dpkg --root=… --force-not-root
--force-script-chrootless`), because this dev box is itself a from-source
shepherd install and chrootless maintainer scripts would act on its real `/etc`.

1. **Synthetic transition test** — established what dpkg actually does for each
   of the three transitions (conffile → plain shipped file; conffile → shipped
   at a new path; conffile → written by the postinst), and that `rm_conffile`
   clears the leftovers. Worth knowing: a conffile that becomes an ordinary file
   at the *same* path is replaced silently, with the conffile record dropped —
   no helper needed for `shepherd.conf`.
2. **Real package upgrade** — built the pre-change `.deb` from a worktree at
   `HEAD` and the post-change one at a bumped version, then upgraded 0.4.1 →
   0.4.2 in the fake root, having first rewritten the drop-in the way 0.4.1's
   postinst does and hand-edited the sway config the way an operator might.
   Result: no prompt; both `/etc` rules gone and present under `/usr`; the sway
   config replaced, admin edit and all; one obsolete conffile record left, the
   drop-in's, as designed. (Maintainer scripts were reduced to their
   `rm_conffile` loops for this run — the rest would have mutated the host.)
3. **Drop-in rendering** — the postinst's block run verbatim against a stubbed
   `systemctl cat` shaped like the real thing (vendor unit first, then a drop-in
   from an earlier run). Confirmed it picks the daemon rather than reading its
   own `ExecStart` back, is idempotent across re-runs, removes the drop-in and
   its directory when there is no `bluetooth.service`, and leaves no `.new` file.
4. **Install/uninstall symmetry** — `install_system` then `uninstall_system`
   under one `DESTDIR`: every staged file is removed, the new template included.
5. `cargo test --workspace --all-targets`, `cargo clippy --workspace
   --all-targets -- -D warnings`, `cargo fmt --all`, and `shellcheck` over the
   scripts, all clean.

A CI step in the `package` job now asserts the built `.deb` declares no
`conffiles` and ships no `bluetooth.service.d/` drop-in. Both assertions were
checked against the old package to confirm they discriminate.

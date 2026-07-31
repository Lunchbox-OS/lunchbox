# Ship the DPC apk through one install path (#2)

<https://git.armeafamily.com/albert/shepherd-launcher/issues/2>

## Prompt

> what's the DPC installation story at this point

...then, after the walkthrough surfaced a gap:

> make it so that it's built into the deb, ready for the admin script to
> install. for source installations, the admin script should expect it to
> already be built; otherwise prompt the user to build it

## The gap

`shepherd-admin apps install android` resolves the DPC apk through
`get_data_dir()`, which is `/usr/share/shepherd` for any install that isn't a
source checkout. Only `package.sh` put the apk there — `install_system` (the
shared "what a system install places" list) didn't. So a **from-source install**
ran `shepherd-admin` out of `/usr/local/bin`, looked in `/usr/share/shepherd`,
found nothing, and told the operator to run `dpc-waydroid/build.sh` — which
writes the apk somewhere the installed admin still wouldn't look.

## What changed

**One staging path.** `install_dpc_apk()` in `scripts/lib/install.sh` copies
`dpc-waydroid/shepherd-dpc.apk` (+ the `.version` sidecar) into
`$SHEPHERD_DATA_INSTALL_DIR`, and `install_system` calls it. Since the `.deb`
stages through `install_system` under `DESTDIR`, the package and a from-source
install now place the apk identically; `package.sh`'s bespoke copy is deleted.
`uninstall_dpc_apk` mirrors it (and deliberately does *not* touch a device owner
already provisioned inside Android — that's `dpm remove-active-admin`).

**It never builds.** The apk is signed with a persistent key: a device that has
the DPC as owner only accepts updates signed with the same key. Auto-building
during packaging would silently mint a throwaway keystore and produce a .deb
that can never update a real device. So building stays explicit
(`dpc-waydroid/build.sh`; the release `deb` job runs it with the org key), and a
missing apk is a **warning** at install time — the Lock Task backend is opt-in
and everything else installs fine — and a **hard stop with instructions** at
provisioning time.

**Two prompts, because the remedy differs.** `install_dpc` now branches on
whether the data dir looks like a checkout (`Cargo.toml` present, matching
`shepherd-admin`'s own heuristic): in-tree it says "build it and re-run"; on an
installed host it says the install shipped without it and gives the
build-then-`./scripts/shepherd install dpc` route. That command is source-only
on purpose — the `.deb` ships `shepherd-admin`, not the `shepherd` CLI — and it
works regardless of how the rest was installed, because the data dir is a fixed
path, not `--prefix`-relative.

**Constants moved to `common.sh`** (`SHEPHERD_DATA_INSTALL_DIR`,
`DPC_APK_NAME`), since the installer and the provisioner have to name the same
file and `install.sh` doesn't source `waydroid.sh`.

`shepherd install dpc` / `shepherd uninstall dpc` are exposed as subcommands so
"build it, then stage it" is a one-liner rather than a re-run of `install all`.

## Bug found while testing

`install_dpc`'s two version lookups were unsafe under `set -euo pipefail`:

```sh
[[ -f "$apk" ]] && apk_ver="$(…)"                       # test false -> AND-list returns 1 -> exit
installed_ver="$(… | grep -oE 'versionName=…' | …)"     # no match -> pipefail -> exit
```

Either one aborted the whole script **silently, rc=1**, before any message could
print. That made the existing "DPC apk not found" `die` unreachable — exactly
the case this change is about — and, worse, the `grep` one fires on the *first*
provision of any device, where the DPC isn't installed yet and `versionName`
matches nothing. Fixed with an `if` and `|| true`; both reads are "absent is an
answer, not an error".

## Verification

- `install_dpc_apk` under `DESTDIR` stages `usr/share/shepherd/shepherd-dpc.apk`
  + `.version`; with the apk moved aside it warns and returns 0.
- `./scripts/shepherd install dpc` / `uninstall dpc` round-trip through a
  `DESTDIR` tree.
- **`./scripts/shepherd package deb --no-build`** → `dpkg-deb -c` shows
  `./usr/share/shepherd/shepherd-dpc.apk` and its sidecar in the package.
- Both missing-apk prompts print and exit 1 (stubbed session + `waydroid_shell`);
  the found-apk path proceeds to the real `pm install`, and a matching
  `.version` short-circuits to "already installed / already device owner".
- CI's shellcheck invocation is clean.

## Follow-up: packaging without the apk is now fatal

> make the release deb fail by default (overrideable with a flag) if the DPC apk
> is missing

Previously a missing apk was only a warning at package time, so a release could
ship without the Lock Task backend and nothing would say so until an operator
ran `apps install android` on a device — the worst place to find out.

- **`shepherd package deb` dies** when `dpc-waydroid/shepherd-dpc.apk` is
  absent, with the build command and the override in the message.
  `--allow-missing-dpc` downgrades it to a warning. Checked *before* the release
  build, so a local run fails in a second rather than after a full compile — and
  forwarded through the unprivileged fakeroot re-exec, which would otherwise
  re-check without the flag and fail an explicitly-allowed build.
- **CI's `package` smoke job** passes `--allow-missing-dpc`: it runs in the base
  image (no Android SDK), publishes nothing, and the apk's own build is covered
  by the `dpc` job.
- **The release job keeps the default and fails.** Its "Build + sign the DPC
  apk" step now errors when `SHEPHERD_KEYSTORE_B64` is unset instead of
  continuing, so the diagnosis lands on the real cause (missing secret) rather
  than on packaging. A deliberate DPC-less release is the new
  `allow_missing_dpc` `workflow_dispatch` input, which gates both that step and
  the `--allow-missing-dpc` flag. A tag push has no inputs, so it always takes
  the strict path.

Verified all three: default + apk present → `.deb` contains the apk; default +
apk moved aside → refuses, rc=1; `--allow-missing-dpc` + apk moved aside →
builds a `.deb` with zero `shepherd-dpc.apk` entries (and the flag survives the
fakeroot re-exec). Both workflow files still parse; shellcheck clean.

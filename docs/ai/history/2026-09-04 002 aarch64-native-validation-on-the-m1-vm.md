# Validating aarch64 natively on the M1 VM (#166)

## Prompt

> keep working on #166 -- you're on the M1 machine now

Tier 2 of the three-tier plan in
[`2026-09-04 001 aarch64-cross-compilation-scope.md`](./2026-09-04%20001%20aarch64-cross-compilation-scope.md):
the M1 VM exists to cover what cross-compiling cannot buy. This is what it
found, plus the repo plumbing that could be written and validated here.

Host: Ubuntu 26.04.1, `aarch64`, 6 cores / 7 GB, kernel 7.0.0-30-generic,
systemd as PID 1.

## tl;dr

**The workspace already runs natively on aarch64, and nothing had to change to
make it.** 1015 unit/integration tests pass, the e2e suite passes with the
IPC peer check *required*, clippy is clean at `-D warnings`, the full
sway + shepherdd + launcher + HUD stack boots headless and paints correctly,
`shepherd package deb --arch arm64` produces a well-formed arm64 package, and
**the eBPF cgroup firewall loads into this kernel and filters real packets**.

That retires most of the risk the scope doc listed as "built, not validated",
and it answers the unverified-link-step question from a different direction
than planned: rather than cross-linking on amd64 and running the result here,
the binaries were linked *and* run here.

| Check | Result |
|---|---|
| `cargo test --workspace --all-targets` | 1015 passed, 0 failed, 20 ignored |
| `cargo test -p shepherd-e2e -- --include-ignored` (`SHEPHERD_REQUIRE_PEER_CGROUP=1`) | 15 passed, 0 failed |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `shepherd dev headless` + `dev shot` | launcher grid + HUD paint correctly |
| `shepherd package deb --arch arm64` | `shepherd-launcher_0.4.1_arm64.deb`, valid |
| `deps install cross` + `build --arch` + `package deb --arch` (→ amd64) | **cross link proven**, valid `..._amd64.deb` |
| `firewall_cgroup` (writable + read-only cgroupfs), as root | passed, real packets filtered |
| `firewall_real` (polkit + helper + systemd-run), as `tester` | passed |
| `firewall_real_flatpak` / `_snap` | skipped — no flatpak, no probe snap |

## What each check settles

### The test suites

`cargo test --workspace --all-targets` is green with no source changes. Worth
naming explicitly, because these are the ones a cross build can never run:
`shepherd-store`'s bundled SQLite, `shepherd-config`'s parsing, the RPC codegen
drift check, and the wire-schema tests all pass on a big-endian-agnostic but
different-integer-width, different-`c_char`-signedness target.

The e2e suite ran with `SHEPHERD_REQUIRE_PEER_CGROUP=1`, so the socket peer
check from #144 is not merely skipping quietly on this kernel — it works.

### Clippy answers an open question in the issue

The issue lists `cargo clippy --target …` as *optional*, on the reasoning that
the workspace has no `cfg(target_arch)` gates outside the wasm crate, so a
second target may catch nothing. Running it natively on aarch64 produced **zero
findings** — no integer-width lints, no `c_char`-signedness lints. That is
evidence for dropping the optional clippy leg from the cross job rather than
paying for it every PR.

### The UI stack

`shepherd dev headless --no-build` boots and `dev tree` reports
`org.shepherd.launcher` mapped and focused; the screenshot shows the launcher
grid with both example entries, their icons, and a fully painted HUD bar
(clock, volume slider at 40%, session state). GTK4, gtk4-layer-shell, the
pixman/`GSK_RENDERER=cairo` path, and sway/wlroots all work on arm64.

Note the caveat this does *not* lift: the headless session forces software
rendering, so nothing here says anything about a real arm64 GPU, and
`install_media_deps`' VA-API driver selection remains untested on this
architecture (and, per the scope doc, has no answer on the eventual Odin 2
Mini either).

### The first arm64 `.deb`

```
Package: shepherd-launcher
Version: 0.4.1
Architecture: arm64
Depends: sway, swayidle, xdg-desktop-portal-wlr, wl-mirror, libgtk-4-1,
         libadwaita-1-0, libgtk4-layer-shell0, libudev1, mpv, python3-venv,
         brightnessctl, bluez
```

`ar t` shows `control.tar.xz` and `data.tar.xz` — the `-Zxz` pin that Forgejo's
Debian registry needs — and the staged binaries are
`ELF 64-bit … ARM aarch64`. The `Depends:` line needed no per-arch handling,
confirming the scope doc's survey of `run.pkgs` against ports.

**The packaging path itself needed no changes to do this.** That is
`scripts/ci/check-arch-neutral.sh` paying off: it has guarded a claim
("`shepherd package deb` works on any host") that nothing had ever exercised,
and the claim turns out to be true.

## The gotcha, demonstrated

The scope doc predicted that `CARGO_BUILD_TARGET` would silently retarget the
eBPF build, because `crates/shepherd-firewall-helper/build.rs` strips
`RUSTUP_TOOLCHAIN`, `CARGO`, `CARGO_TARGET_DIR` and `RUSTFLAGS` from the child
`cargo` but not that one. Confirmed by removing the new `.env_remove` line and
exporting the variable: the BPF crate builds into
`crates/shepherd-firewall-bpf/target/aarch64-unknown-linux-gnu/` and dies on a
link error. With the strip in place the object stays
`ELF 64-bit LSB relocatable, eBPF`.

The cross build therefore passes `--target` on the command line, which never
reaches the child at all; the `env_remove` is the belt to that braces, for
anyone who cross-compiles the obvious way instead.

## The firewall BPF suites: the check this VM existed for

These are the reason tier 2 is a native machine rather than a cross build. The
verifier runs on the *target* kernel, and #151 is the standing reminder that
"it linked" and "the kernel accepts it" are different claims. On kernel
7.0.0-30-generic, aarch64:

- **`firewall_cgroup`** (the #151 path — drives `apply-cgroup` directly, loads
  the embedded BPF object, checks that packets really are filtered) passes as
  root with `SHEPHERD_FIREWALL_CGROUP_REQUIRED=1`:
  `cgroup firewall filtered as configured (allow=127.0.0.1, deny=192.168.64.12)`.
- **The read-only-`/sys/fs/cgroup` fallback** passes too, run the way `ci.yml`
  does it, under `unshare -m --propagation private` with the hierarchy
  remounted read-only. The log confirms it really took the fallback
  (`creating cgroups through a private cgroup2 mount at …`) rather than finding
  a writable cgroupfs after all.
- **`firewall_real`** — the whole privileged path: polkit authorisation, the
  installed helper, a `systemd-run` scope, real filtering — passes as a
  non-root `tester` user in the `shepherd-firewall` group: `allow=OPEN`,
  `deny=BLOCKED`.

So the BPF object this workspace emits is accepted and enforced by an arm64
kernel. That was the largest single unknown in the issue.

`firewall_real_flatpak` and `firewall_real_snap` skipped explicitly (no
flatpak CLI; no probe snap). CI's firewall job does not run them either, and
what they cover — cgroup *path* shapes for flatpak and snap — is
architecture-independent, so provisioning them here would buy nothing.

### How to re-run them here

`sudo` was the only obstacle, for the reason in the CONTRIBUTING note: a
`NOPASSWD` rule listed *before* the `%sudo` group rule loses, because sudo
applies the last match.

To avoid leaving root-owned files in `target/`, build as the normal user and
run the **test binary** under sudo rather than running `cargo` as root:

```sh
cargo test -p shepherd-e2e --test firewall_cgroup --no-run   # prints the path
sudo env SHEPHERD_FIREWALL_CGROUP_REQUIRED=1 \
    ./target/debug/deps/firewall_cgroup-<hash> \
        --include-ignored --test-threads=1 --nocapture
```

CI instead `chown -R`s the whole workspace to `tester`, which it can afford
because the checkout is disposable.

### State this left on the VM

`firewall_real` needs a real install, so the following is now present and is
*not* cleaned up (re-running the suite would only have to redo it). All of it
is reversible:

- `/usr/libexec/shepherd-firewall-helper`
- `/usr/share/polkit-1/actions/org.shepherd.firewall.policy`
- `/etc/polkit-1/rules.d/50-shepherd-firewall.rules`
- the `shepherd-firewall` system group
- a `tester` user, in that group, plus an ACL granting it traversal of
  `/home/shepherd-dev`

## The cross link, proven (in the mirror direction)

The one step nothing had ever executed. The original probe died on ENOSPC
before linking; native aarch64 builds link, but never *cross*. Once Canonical's
archive came back it became testable here, because `us.archive.ubuntu.com`
serves both architectures — so this host can cross-compile **towards amd64**,
the mirror image of what CI does.

```sh
./scripts/shepherd deps install cross --arch amd64
./scripts/shepherd build --arch amd64
./scripts/shepherd package deb --arch amd64 --out dist/pkg
```

All three succeeded, first attempt, no source changes. Results:

- **Every binary linked and is `ELF 64-bit … x86-64`** — all eight session
  binaries plus `shepherd-firewall-helper`, interpreter
  `/lib64/ld-linux-x86-64.so.2`, and `NEEDED` entries resolved against the
  amd64 sysroot (`libgtk-4.so.1`, `libudev.so.1`, …). The workspace cross-links,
  and the last open question from the scoping doc is closed.
- **The eBPF object stayed `ELF … eBPF`,** and
  `crates/shepherd-firewall-bpf/target/` contains only `bpfel-unknown-none`
  with no host-triple directory beside it. That is the `--target`-on-the-
  command-line decision holding up in a real cross build rather than in the
  synthetic test that first demonstrated it.
- **`package deb --arch amd64` produced a valid package**: `Architecture:
  amd64`, `control.tar.xz` + `data.tar.xz`, and x86-64 binaries staged inside
  it — i.e. the label and the contents agree, which is what the new CI
  assertions check.
- **The native path is undisturbed.** A plain `shepherd build` afterwards still
  produces `ARM aarch64` binaries in `target/debug`, and `deps check dev` still
  passes. The cross build lives entirely in `target/x86_64-unknown-linux-gnu/`.

What this does **not** prove: the ports-mirror fallback (this host's mirror
serves both, so `deps install cross` reported "already serves amd64; no ports
entry needed" and rewrote nothing), and anything about *running* the result —
these are amd64 binaries on an arm64 machine.

### The cross set is not co-installable with the native one — a correction

The scoping doc's probe concluded that the target architecture's `-dev`
packages "are all co-installable with the amd64 host set and none is missing
from ports — the apt step exited 0." **The exit code was the wrong thing to
read.** `apt-get install -y` resolves a conflict by *removing* the offending
packages and still exits 0.

Doing it here removed eight natively-installed packages:

```
libmpv-dev libarchive-dev libcdio-dev libcdio-cdda-dev
libcdio-paranoia-dev libext2fs-dev libgirepository1.0-dev libtool-bin
```

`libmpv-dev` is `Multi-Arch: same`, but it depends on `libcdio-dev`,
`libext2fs-dev`, `libgirepository1.0-dev` and `libtool-bin`, which are
`Multi-Arch: no` — so the whole chain is exclusive, and a host can have one
architecture's or the other's, not both. `apt-get install --no-remove` confirms
it from the other side: restoring the native set requires removing the target
one.

The consequence was quiet and remote, exactly as feared. The next
`cargo test --workspace` failed to link `shepherd-media-android` with
`cannot find -lmpv` — a message that says nothing about cross-compiling, on a
machine whose native build had been green an hour earlier.

`deps install cross` now simulates the install first and refuses when anything
native would go, naming the packages and offering the two real options
(cross-compile in a container, or `--allow-remove` and restore with
`deps install build`). `.ci/Dockerfile.cross` passes `--allow-remove`, which is
safe there specifically: that image only ever cross-compiles, so it has no
native build to protect.

This does not weaken the cross-link result above — that build linked all nine
binaries against a complete target sysroot. It changes where cross builds
should *happen*: a container, not a workstation you also build natively on.

### One alarming-looking thing that is fine

Installing a foreign architecture's libraries runs their `postinst` scripts,
and several try to execute a helper of that architecture:

```
/var/lib/dpkg/info/libglib2.0-0t64:amd64.postinst: 41:
  /usr/lib/x86_64-linux-gnu/glib-2.0/glib-compile-schemas: Exec format error
```

Five of those appeared (glib schemas, gio modules, gdk-pixbuf loaders). They
are expected on any multiarch host without an emulator, do not fail the
install, and matter only to *running* that architecture's software — which is
not what a sysroot is for. `apt` exited 0 and the sysroot is complete.

## Two setup gaps this VM exposed

- **`deps install dev` does not install clippy or rustfmt.** `install_rust`
  runs rustup with `--profile minimal`, and nothing adds the components back.
  `.ci/Dockerfile` has carried `RUN rustup component add clippy rustfmt` for
  exactly this reason, with a comment saying so — so CI is fine and a developer
  following CONTRIBUTING is not. `deps check dev` reports "all installed" while
  `cargo clippy` fails with "'cargo-clippy' is not installed".
- **`dist/pkg/` was not gitignored**, so a local `shepherd package deb` leaves
  an untracked 13 MB artifact. Added.

## Repo changes made here

All three are architecture-neutral and were validated on this host:

1. **`scripts/lib/build.sh`** — `get_target_dir` honours a triple; `--arch` /
   `--target` on `shepherd build`; `arch_to_triple` via `dpkg-architecture`,
   checked against `rustc --print target-list`; the cross environment exported
   only for a foreign triple and only where unset.
2. **`scripts/lib/package.sh`** — `--arch` on `shepherd package deb`, defaulting
   to `dpkg --print-architecture`, and surviving the fakeroot re-exec.
3. **`scripts/deps/cross.pkgs` + `deps install cross --arch`** — with
   `scripts/ci/check-cross-pkgs.sh` keeping it in step with `build.pkgs`.

An `--arch` naming the host's own architecture deliberately stays a *native*
build (triple unset, existing output paths and fingerprints). That is what lets
a CI matrix pass `--arch` on every leg without giving the native leg a second,
redundant `target/` directory.

### What the mirror probing found — and a correction

`_deps_enable_foreign_arch` decides whether a fallback source is needed by
probing `dists/<suite>/<component>/binary-<arch>/Release` on the configured
mirror, rather than encoding Ubuntu's archive/ports split. Three reasons, the
third of which only became clear after this was written:

1. The split would be an `amd64` literal in a file `check-arch-neutral.sh`
   scans.
2. The suite `Release` file cannot answer the question. Its `Architectures:`
   field lists everything the *suite* defines (`amd64 arm64 armhf ppc64el
   riscv64 s390x …`) regardless of what the mirror holds, and every mirror
   returns the identical list. The per-arch binary index is the discriminator.
3. **The split is no longer true.** On 26.04, `archive.ubuntu.com` serves
   arm64: `dists/resolute/main/binary-arm64/Release` returns `Architecture:
   arm64` with a real `Packages.gz` behind it. Ports still 404s for amd64, so
   it remains a genuinely non-primary mirror — but "arm64 lives on ports" is
   stale.

**This corrects two earlier claims in this branch**, both made while
Canonical's archive was mid-outage and could not be probed:

- The commit that added `deps install cross` says archive.ubuntu.com is "the
  special case, not the rule". It is not a special case; it carries both.
- The CI commit predicted that the container — whose mirror is
  archive.ubuntu.com — would take the ports fallback, and that CI would
  therefore exercise that branch for the first time. **It will not.** The probe
  will find archive already serves arm64, and `deps install cross` will add no
  source and rewrite nothing.

That is a better outcome than predicted (the riskiest branch is not on the CI
path at all), but it means **the ports fallback is exercised nowhere** — not
here, not in CI. It stands as a safety net for a partial mirror, unproven.

The episode is also the argument for the probe. A hardcoded "arm64 → ports"
rule would have been written against a world that had already changed, and
would now be rewriting a perfectly good sources file to add a redundant mirror.

## What is left on #166

The CI pieces are now written but **unrun** — they could not be exercised from
this VM, since the cross build happens on the amd64 runner:

- `.ci/Dockerfile.cross` + the `image-cross` job in `images.yml` (hash covers
  `Dockerfile.cross`, `cross.pkgs`, `deps.sh`, the base ref and `TARGET_ARCH`).
- `ci.yml`: a `build-arm64` job, and `package` matrixed over both arches.
- `release.yml`: the `deb` job matrixed over both arches.

Both of the risks this section originally listed have since been retired: the
cross link is proven (above), and the mirror question is settled (the
container's `archive.ubuntu.com` serves arm64, so `deps install cross` will add
no source and rewrite nothing).

What is left is genuinely CI-shaped and cannot be rehearsed here: whether the
`image-cross` build succeeds on the runner, whether the DinD/registry plumbing
copied from `image-android` works for a third image, and how much the extra
image job costs the critical path.

Also still open:

- The optional native-arm64 workflow for this VM (`workflow_dispatch` +
  `schedule`, own runner label, never referenced from `ci.yml`) — this machine
  is not yet registered as a Forgejo runner.
- `docs/INSTALL.md`, once `release.yml` actually publishes an arm64 package —
  it should not advertise one before then. `CONTRIBUTING.md` and
  `scripts/README.md` are done.
- Installing the published arm64 `.deb` on the Pi 4 and booting it.

### Notes for whoever runs the first CI pass

- The `images` reusable workflow now has a third job, and every job that
  `needs: images` waits for the whole workflow. `image-cross` runs in parallel
  with `image-android` (both `needs: image`), so the critical path only grows
  if the cross layer is slower than the SDK download — but on a hash miss both
  are minutes, and a `deps.sh` change invalidates all three images at once.
- The `package` cache key gained an arch segment, so the amd64 leg takes one
  cold `target/` cache on the first run after this lands.
- `Dockerfile.cross` derives the triple with `sed`, not `${var/a/b}`: Docker
  runs each `RUN` through `/bin/sh`, which is dash on Ubuntu, and dash has no
  such substitution. The first draft of that file used the bash form and would
  have failed the image build with `Bad substitution`.

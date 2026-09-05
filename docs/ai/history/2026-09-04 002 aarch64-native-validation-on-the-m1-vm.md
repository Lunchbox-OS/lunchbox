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

### One thing the mirror probing found

`_deps_enable_foreign_arch` decides whether a ports source is needed by probing
`dists/<suite>/<component>/binary-<arch>/Release` on the configured mirror. Two
reasons it does not just encode Ubuntu's archive/ports split:

- It would be an `amd64` literal in a file `check-arch-neutral.sh` scans.
- **It would be wrong.** This VM's mirror (`mirrors.mit.edu`) serves `arm64`
  *and* `amd64`, so it needs no ports entry at all. Ubuntu's own
  `archive.ubuntu.com` is the special case, not the rule.

The suite `Release` file cannot answer the question — its `Architectures:`
field lists everything the *suite* defines (`amd64 arm64 armhf ppc64el riscv64
s390x …`) regardless of what the mirror holds, and both mirrors return the
identical list. The per-arch binary index is the discriminator: present on
`ports.ubuntu.com` for `arm64`, 404 there for `amd64`.

The ports fallback branch is therefore **unvalidated** — this host's mirror
never takes it, and Canonical was mid-outage during the session, so
`archive.ubuntu.com` could not be probed as a counter-example either.

## What is left on #166

Unchanged from the issue, minus what is above. The remaining work is all on the
amd64 side, where it can actually be exercised:

- `.ci/Dockerfile.cross` and the `image-cross` job in `images.yml` (its content
  hash must include `Dockerfile.cross` and `cross.pkgs`).
- `ci.yml`: a `build-arm64` job, the `package` job matrixed over both arches,
  arm64-specific `target/` cache keys. The optional clippy leg now looks
  droppable — see above.
- `release.yml`: matrix the `deb` job, with `name: Build .deb (${{ matrix.arch }})`
  because `check-workflows.sh` rejects duplicate job names.
- The optional native-arm64 workflow for this VM (`workflow_dispatch` +
  `schedule`, own runner label, never referenced from `ci.yml`).
- `docs/INSTALL.md`, once `release.yml` actually publishes an arm64 package —
  it should not advertise one before then. `CONTRIBUTING.md` and
  `scripts/README.md` are done.

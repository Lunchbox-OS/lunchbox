# Scoping aarch64 (arm64) builds, here and in CI

## Prompt

> scope out what it would take to get this cross-compiling for aarch64, both
> here and in CI

Scoping only — nothing in this pass was implemented. The follow-up from
[`2026-07-04 003 binary-releases.md`](./2026-07-04%20003%20binary-releases.md)
("arm64 `.deb`", listed under *Open follow-ups*) has never been filed as an
issue; the issue tracker has no arm64/aarch64 issue as of this writing.

## tl;dr

Cross-compiling the workspace for `aarch64-unknown-linux-gnu` is **less work
than the 2026-07-04 doc assumed**. A live probe (below) cross-compiled the
entire dependency graph — GTK4, libmpv, libdbus, libudev, bundled SQLite,
eframe/egui — against an Ubuntu 26.04 arm64 multiarch sysroot with **zero
source changes** and four environment variables. Everything that is left is
plumbing:

| Area | Work |
|---|---|
| `scripts/lib/build.sh` | target-aware `get_target_dir` + a `--target`/`--arch` flag |
| `scripts/lib/package.sh` | `--arch` instead of `dpkg --print-architecture` only |
| `scripts/deps/` | a cross package set + `deps install cross` |
| `crates/shepherd-firewall-helper/build.rs` | one `env_remove` line |
| `.ci/` + `images.yml` | a third CI image (cross toolchain + arm64 sysroot) |
| `ci.yml` / `release.yml` | an arm64 build job; matrix the `.deb` jobs over arch |

Estimated **2–3 days** to "CI builds and publishes an arm64 `.deb`", of which
about a day is CI-image iteration. What cross-compiling *cannot* buy is test
coverage: no arm64 test, e2e, firewall or headless run comes with it.

Given the hardware on hand (an M1 Pro laptop and a Raspberry Pi 4), the
decision is to **cross-compile for the per-PR gate, validate on an M1 arm64
VM, and keep the Pi 4 as a release smoke target rather than a CI runner**. See
*Decision* below.

## Probe: does the native surface actually cross?

Run locally in a throwaway `ubuntu:26.04` container (amd64 host), not on the
runner. Recipe:

1. Rewrite `/etc/apt/sources.list.d/ubuntu.sources` (deb822 on 26.04) to pin
   the archive/security entries to `Architectures: amd64` and add a
   `http://ports.ubuntu.com/ubuntu-ports/` entry with `Architectures: arm64`.
   The main mirrors carry no arm64; ports does.
2. `dpkg --add-architecture arm64 && apt-get update`.
3. `apt-get install crossbuild-essential-arm64` plus the arm64 halves of
   `scripts/deps/build.pkgs`: `libglib2.0-dev libgtk-4-dev libadwaita-1-dev
   libcairo2-dev libpango1.0-dev libgdk-pixbuf-xlib-2.0-dev libwayland-dev
   libxkbcommon-dev libudev-dev libx11-dev libgirepository1.0-dev
   libgtk4-layer-shell-dev libmpv-dev libdbus-1-dev`, each `:arm64`. **All of
   them are co-installable with the amd64 host set and none is missing from
   ports** — the apt step exited 0.
4. `rustup target add aarch64-unknown-linux-gnu`.
5. Build with:

   ```
   PKG_CONFIG_ALLOW_CROSS=1
   PKG_CONFIG_LIBDIR=/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig
   PKG_CONFIG_SYSROOT_DIR=/
   CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
   cargo build --locked --target aarch64-unknown-linux-gnu \
     -p shepherdd -p shepherd-hud -p shepherd-launcher-ui -p shepherd-media \
     -p shepherd-pairing-display -p shepherd-gamepad-bridge \
     -p shepherd-touch-bridge -p shepherd-ble -p shepherd-store
   ```

`PKG_CONFIG_LIBDIR` is needed because Ubuntu 26.04 has **no
`pkg-config-aarch64-linux-gnu` package** (the binary now comes from
`pkgconf-bin`). `crossbuild-essential-arm64` does drop
`/usr/bin/aarch64-linux-gnu-pkg-config` on PATH, so `PKG_CONFIG` pointing at
that wrapper is the alternative; the explicit `LIBDIR` was used here and works.

### What the probe proved

Every `-sys` build script resolved arm64 `.pc` files and every crate in the
graph compiled for aarch64:

- **GTK4 stack** — `glib-sys`, `gobject-sys`, `gio-sys`, `cairo-sys`,
  `pango-sys`, `gdk-pixbuf-sys`, `graphene-sys`, `gdk4-sys`, `gsk4-sys`,
  `gtk4-sys`, and `gtk4-layer-shell-sys` (via `system-deps`) all emitted
  `-L native=/usr/lib/aarch64-linux-gnu`.
- **libmpv** — `libmpv2-sys` uses its pregenerated bindings on Linux (only the
  Android build sets `use-bindgen`). Those bindings spell every scalar as
  `::std::os::raw::*`, so the `c_char` signedness difference on aarch64 is a
  non-issue; the crate just emits `cargo:rustc-link-lib=mpv`.
- **libdbus** (`bluer` → `dbus` → `libdbus-sys`) — `shepherd-ble` compiled.
- **libudev** (`gilrs` → `libudev-sys`) — `shepherd-gamepad-bridge` compiled.
- **bundled SQLite** (`rusqlite`/`libsqlite3-sys`) — the `cc` crate picked
  `aarch64-linux-gnu-gcc` off the target triple with no configuration;
  `shepherd-store` compiled.
- **eframe/egui/glow** — `wayland-sys` and `x11-dl` `dlopen` at runtime, so
  they contribute nothing to the link.

### What the probe did *not* prove

The run died on **`No space left on device`** — the host filesystem is at
~98% (the repo's own `target/` is 60 GB, docker images another 10 GB), and the
container's writable layer filled during codegen. It got as far as compiling
`shepherd-media` and `gtk4` themselves, so the only unverified step is the
**final link** of each binary. That step is `aarch64-linux-gnu-gcc` consuming
the `-l`/`-L` flags the build scripts already emitted correctly, so it is the
least likely thing to surprise — but it is not yet green. Re-running needs a
few GB of headroom.

### Runtime dependencies exist on arm64

Checked every entry in `scripts/deps/run.pkgs` against ports for resolute:
`sway 1.11-3`, `swayidle 1.9.0-1`, `xdg-desktop-portal-wlr 0.8.1-1`,
`wl-mirror 0.18.5-1`, `libgtk-4-1`, `libadwaita-1-0`,
`libgtk4-layer-shell0 1.3.0-1`, `libudev1`, `mpv 0.41.0-2ubuntu4`,
`python3-venv`, `brightnessctl`, `bluez 5.85-4ubuntu0.1`. Nothing missing, so
the generated `Depends:` line needs no per-arch special-casing.

## Three routes, and which to take

**A. Native arm64 runner.** A second Forgejo runner on real arm64 hardware.
Needs *no* repo changes at all — `deps install build`, `shepherd build` and
`shepherd package deb` are already arch-neutral and would produce a correct
arm64 `.deb` on an arm64 host (`dpkg --print-architecture` does the right
thing). CI cost is a second base image built for arm64. This is the only route
that runs the test/e2e/firewall suites on the target architecture.

**B. Cross from amd64 (recommended for the `.deb`).** What the probe
validated. Fast — an arm64 build costs about what the amd64 build costs — and
it fits the existing runner. Costs script changes (below) and buys no tests.

**C. Emulated arm64 (qemu-user via binfmt).** No script changes whatsoever;
`cargo test` works because the emulator runs the binaries. But it is ~5–10×
slower, and it needs `binfmt_misc` registered on the *runner host* — this dev
box has no qemu handler registered today, and the Forgejo runner's job
containers are non-privileged (see the lengths `ci.yml`'s `firewall` job goes
to for a privileged sidecar), so registering it is a host-level change.

**Recommendation:** B for the CI gate, with the available hardware filling the
gaps it leaves. See the next section — the answer turns on what machines exist,
not on the routes in the abstract.

## Decision: which route, given the hardware on hand (2026-09-04)

Available aarch64 hardware:

- **MacBook Pro (M1 Pro)** — can host an Ubuntu 26.04 arm64 VM. Fast (it will
  beat the amd64 runner, whose job containers are capped at `--cpus=2
  --memory=2g` per `2026-05-02 006 ci image cache.md`), but it is a laptop:
  not always up, so nothing required may depend on it.
- **Raspberry Pi 4** — the only always-on-capable aarch64 box.

The decision is a three-tier split, matching each machine to what it is good
at:

### 1. Per-PR gate — cross-compile on the existing amd64 runner (route B)

The only arm64 build that can be a *required* check, because it is the only one
running on hardware that is always up. Marginal cost is roughly one more build
job on a pipeline that is already running. Catches what actually recurs: a new
dependency that does not cross, an arch-specific compile error, packaging
breakage.

### 2. Correctness — the M1 VM, native aarch64

Covers exactly what cross-compiling cannot: `cargo test --workspace`, the e2e
suite, and the firewall BPF tests, where the kernel verifier is the authority
(cf. issue #151). Wire it as an **optional** runner behind its own label and
its own workflow (`workflow_dispatch` + `schedule`) — never in `ci.yml`. A
sleeping laptop must not leave a PR check pending. (`release.yml` already uses
`workflow_dispatch` for its dry run, so the pattern is established here.)

The M1 VM is also what closes the gap this doc's probe left open: cross-build
on amd64, run the resulting binary in the VM, and the unverified link step plus
the runtime behaviour are settled together.

### 3. Pi 4 — release smoke target, not a runner

Install the published arm64 `.deb`, boot the session, screenshot, done.
Minutes, no compiling, no `target/` churning an SD card. If automated later, it
should be a Pi-hosted runner that *only* installs and smokes.

### Why the Pi is not the CI runner (rejecting route A)

As a runner it would have to do everything: the weekly CI-image rebuild — whose
cost is dominated by `cargo install bpf-linker`, a from-source LLVM-linked
build that already takes 5–8 min on the amd64 runner — plus release builds,
tests, and e2e, per PR. The `--memory=2g` container cap is already tight for
rustc on amd64; on a 4 GB Pi with parallel rustc processes that means swapping,
and swapping to SD is where it stops being a CI system. It would also make one
Pi the single point of failure for the release pipeline that publishes to the
apt registry.

The M1 does remove route A's *image-build* blocker (`docker buildx` there
produces a native arm64 image in reasonable time, pushable to the Forgejo
registry). That is worth remembering if route A is ever revisited — but it does
not address the per-PR compile cost, so it does not change the decision.

### The alternative that was considered and rejected

Skip the cross plumbing entirely and build arm64 `.deb`s by hand on the M1:
zero days of work against ~2–3. Rejected because the release pipeline is
already fully automated tag → build → sign → apt-publish, and hanging one
architecture off a laptop being awake breaks that property.

## Settled: the Pi 4 is an intermediate test target, not the product target

The long-term target device is an **Ayn Odin 2 Mini** — an aarch64 handheld.
Getting a mostly-stock Ubuntu 26.04 booting on it is a substantial piece of work
that lives outside this repo (it likely means building the image and kernel),
and it cannot start until shepherd-launcher has an arm64 build at all. That
makes this work a prerequisite for the device plan rather than a response to it.

The Pi 4 is therefore the aarch64 hardware on hand, not a platform to support.
Two Pi-specific runtime gaps follow from that and should **not** be chased as
part of the build work:

- **Media.** `install_media_deps` (`scripts/lib/admin.sh`) selects a libva
  driver from detected hardware. The Pi has no VA-API — decode is
  v4l2m2m/DRM-prime, and mpv's Pi-specific support has been drifting out of
  favour upstream. On a Pi 4 that function has nothing to select, so
  `shepherd-media` decodes in software: precisely the case the `hwdec-current`
  warning exists to report.
- **BLE.** `2026-07-26 001 ble-advertisement-name-overflow.md` and
  `docs/INSTALL.md` already cite raspberrypi/linux#7473, a Pi kernel bug, and
  issue #109 tracks kernel-version-sensitive BlueZ advertising. Pairing on a Pi
  is a known-rough area rather than a new risk.

Neither blocks the build work. Worth noting for later: the Odin 2 Mini is also
a non-VA-API device (Adreno), so `install_media_deps`' libva-driver selection
has no answer there either. A general non-VA-API decode path is real work that
deserves its own issue — but it is independent of getting an arm64 build, and
should not be folded into it.

## Filed as

An issue body drafted from this doc covers: the three-tier approach, the probe
results, task checkboxes across scripts / deps / CI images / workflows /
release / docs, the gotchas below, and the validation plan.

## Repo changes for route B

### 1. `scripts/lib/build.sh` — target-aware output paths

`get_target_dir` (build.sh:25) is the single choke point: `install.sh:70`
(`install_bins`), `install.sh:515` (the firewall helper) and `package.sh` all
route through it. Teach it about a target triple (an exported
`SHEPHERD_CARGO_TARGET`, or a parameter) so it returns
`target/<triple>/{debug,release}` when one is set, and everything downstream
follows for free.

`build_cargo` then passes `--target "$triple"` and `build_main` grows
`--target <triple>` / `--arch <debian-arch>` (build.sh's `build_main` currently
accepts only `--release`, `clean`, `config-editor`, `config-wasm`).

**Pass `--target` on the command line; do not export `CARGO_BUILD_TARGET`** —
see the firewall-helper note below.

The webui/wasm half of `build_cargo` (`build_webui` → `build_config_wasm` →
npm) is host-side and produces arch-neutral assets. It does not need to change,
but note that a two-arch matrix rebuilds it twice, since Forgejo cannot pass
artifacts between jobs (`upload-artifact@v4` hard-fails there — see
`release.yml`'s header).

### 2. `scripts/lib/package.sh` — an explicit target arch

`package_deb` reads the arch at package.sh:134 (`dpkg --print-architecture`)
and uses it for `Architecture:` (`_package_write_control`) and the filename.
Add `--arch <debian-arch>`, defaulting to the current behaviour, and derive the
Rust triple from it *mechanically* rather than with a lookup table:

```sh
gnu="$(dpkg-architecture -a"$arch" -qDEB_HOST_GNU_TYPE)"   # e.g. aarch64-linux-gnu
triple="${gnu/-linux-gnu/-unknown-linux-gnu}"              # aarch64-unknown-linux-gnu
```

This matters for `scripts/ci/check-arch-neutral.sh`, which fails any line in
the build/packaging path containing `amd64`/`x86_64` unless the same line also
names an arm arch. A hand-written `case` mapping triples to Debian arches would
trip it; the derivation above has no arch literals at all.

### 3. `scripts/deps/` — a cross dependency set

New `scripts/deps/cross.pkgs` (host-side cross toolchain + the `-dev:<arch>`
list from the probe) and a `deps install cross --arch <arch>` path in
`scripts/lib/deps.sh` that also does the `dpkg --add-architecture` and the
deb822 ports-source edit. Note `check-arch-neutral.sh` already scans
`scripts/deps/*.pkgs`, and arm literals are exempt, so `crossbuild-essential-arm64`
is fine as written.

### 4. `crates/shepherd-firewall-helper/build.rs` — one line

The build script shells out to `rustup run nightly cargo build` for the sibling
BPF crate, and deliberately strips the parent's `RUSTUP_TOOLCHAIN`, `CARGO`,
`CARGO_TARGET_DIR`, `RUSTFLAGS` and friends. It does **not** strip
`CARGO_BUILD_TARGET`. That variable would override
`crates/shepherd-firewall-bpf/.cargo/config.toml`'s `[build] target =
"bpfel-unknown-none"` (env beats config), and the child build would try to
compile the eBPF program for aarch64. Add `.env_remove("CARGO_BUILD_TARGET")`
alongside the others; it is correct regardless of whether the cross build ever
lands, and it removes the footgun of anyone cross-compiling by exporting the
variable.

The BPF object itself is architecture-independent (`bpfel-unknown-none` is
64-bit little-endian either way), and `bpf-linker` runs on the host, so nothing
else in the firewall path changes.

### 5. `libudev-sys`'s host probe — a papercut worth knowing

`libudev-sys`'s build script, after the (correctly cross-aware) `pkg_config`
call, compiles a probe program with the **host** `rustc` and `-l udev` to
decide whether to set `cfg(hwdb)`. In a cross container with only
`libudev-dev:arm64` installed, that probe silently fails and `hwdb` is left
off. It is not a build error and nothing in this workspace uses the hwdb
bindings, but install `libudev-dev` for **both** architectures in the cross
image so the cross build and a native build agree.

### 6. Cross environment

Either commit a `[target.aarch64-unknown-linux-gnu]` section to a repo
`.cargo/config.toml` (there is none today) for the linker, and export the three
`PKG_CONFIG_*` variables from the build lib when a target is set — or bake all
four into the cross CI image. Baking them into the image keeps the repo free of
arch literals; exporting them from `build.sh` makes a local cross build work
without the image. Doing both (image sets them, `build.sh` sets them if unset)
is the least surprising.

## CI changes

### `images.yml` — a third image

Add a `image-cross` job mirroring the existing `image` / `image-android` jobs,
building a `.ci/Dockerfile.cross` that layers the cross toolchain and arm64
sysroot onto the base image, and expose a `cross-ref` output. Two things to get
right:

- The image tag hash in `images.yml` is computed over `.ci/Dockerfile`,
  `scripts/deps/{build,run,test}.pkgs` and `scripts/lib/deps.sh`. The cross
  image's own hash must include `.ci/Dockerfile.cross` and `scripts/deps/cross.pkgs`,
  or a dependency change will not trigger a rebuild.
- Size: the arm64 GTK+mpv+dbus dev set is roughly 1.5–2.5 GB on top of the base
  image. Keeping it in a **separate** image rather than extending the base one
  matters — every other job pulls the base image on every run.

### `ci.yml`

- New `build-arm64` job on the cross image: `./scripts/shepherd build --arch arm64`
  (or the raw `cargo build --target …`), plus `cargo clippy --target …
  --workspace --all-targets -- -D warnings` if the extra minutes are acceptable.
  Clippy for a second target catches genuinely different code —
  `cfg(target_arch)`, integer-width and `c_char`-signedness lints — but the
  workspace has no `target_arch` gates outside the wasm crate today, so this is
  optional.
- `package` job: matrix over `[amd64, arm64]` so the `.deb` smoke build (and
  the `-Zxz` assertion) covers both. The arm64 leg needs the cross image and
  `--arch arm64`; the `dpkg-deb -I` and `ar t` assertions are arch-agnostic.
- `arch-neutral` and `workflows` jobs: unchanged, but the new scripts land in
  the scanned set automatically.
- Cache keys: give the arm64 legs their own `target/` key
  (`…-cargo-target-package-arm64-v1-…`); the per-job-key convention already in
  the file exists exactly for this reason.
- `test`, `e2e`, `firewall`, `webui`, `config-editor`, `versions`, the Android
  jobs: unchanged, amd64 only.

### `release.yml`

- The `deb` job is named `Build .deb (amd64)` and hardcodes nothing else about
  the arch. Turn it into a matrix over `[amd64, arm64]` with
  `name: Build .deb (${{ matrix.arch }})` and `--arch ${{ matrix.arch }}`.
  Watch `check-workflows.sh` — it fails on duplicate job names within a file, so
  the matrix interpolation in `name:` is required, not cosmetic.
- `scripts/ci/upload-release-asset.sh` and `scripts/ci/publish-apt.sh` both take
  file globs and need **no change**: Forgejo reads `Architecture:` out of the
  `.deb` control file and serves both arches from one distribution/component,
  which is exactly what
  [`2026-07-19 002 forgejo-apt-repository.md`](./2026-07-19%20002%20forgejo-apt-repository.md)
  anticipated ("arm64 in the repo stays … apt serves both").
- Per-asset `.sha256` sidecars are generated per file, so they matrix cleanly.

## Documentation to update

- `docs/INSTALL.md` — "prebuilt amd64 packages are also …" in the standalone
  `.deb` section, and the example filename `shepherd-launcher_0.2.0_amd64.deb`.
  The apt-repository section needs nothing.
- `CONTRIBUTING.md` — a short cross-build section (`deps install cross`,
  `shepherd build --arch arm64`).
- `scripts/README.md` — the new `deps install cross` and `build --arch`.

## What arm64 support will still not cover

Cross-compiling produces binaries nobody has run. Specifically untested:

- Everything in `cargo test`, `shepherd-e2e`, the firewall cgroup/BPF suites,
  and the headless dev session. The BPF path is the most exposed: the verifier
  runs on the target kernel, and issue #151 (the firewall BPF object
  misalignment) is a reminder that "it linked" and "the kernel accepts it" are
  different claims.
- BLE. `docs/ai/history/2026-07-26 001 ble-advertisement-name-overflow.md` and
  issue #109 already record kernel-version-sensitive BlueZ behaviour, with a
  Raspberry Pi kernel bug (raspberrypi/linux#7473) cited in both that doc and
  `docs/INSTALL.md` — which is precisely the hardware an arm64 `.deb` invites.
- VA-API / `hwdec`. `install_media_deps` picks a libva driver from detected
  hardware; the arm64 SoC drivers (`rkvdec`, V3D, etc.) are a different set from
  the amd64 ones the function knows about.
- sway/wlroots on a real arm64 GPU.

The honest framing for a first arm64 release is "built, not validated", and the
`.deb` should probably ship after someone has booted it once on the intended
device.

## Effort

| Piece | Estimate |
|---|---|
| `build.sh` / `package.sh` / `install.sh` target awareness | 0.5 day |
| `deps install cross` + `cross.pkgs` | 0.25 day |
| `Dockerfile.cross` + `images.yml` job (slow to iterate on the runner) | 0.5–1 day |
| `ci.yml` + `release.yml` matrices | 0.5 day |
| Docs + a history note | 0.25 day |
| Buffer for first-time link/runtime surprises | 0.5 day |
| **Total** | **~2–3 days** |

A native arm64 runner instead (route A) is ~0 days of repo work plus hardware
and one image build, and is strictly better for correctness — it is only worse
for wall-clock CI time and for needing a machine.

## Notes for whoever picks this up

- The dev box this was probed on is at **98% disk** (`target/` alone is 60 GB,
  docker images 10 GB). Free space before re-running the probe, or it will fail
  the same way.
- The probe container recipe above is worth keeping as a scratch
  `Dockerfile.cross` starting point; it is nearly the CI image already.

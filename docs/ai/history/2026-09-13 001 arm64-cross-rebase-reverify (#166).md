# arm64 cross-compilation — rebase onto main and re-verify (issue #166, PR #167)

Prompt: "#167 is currently checked out. Rebase it on the current main and reverify".

PR #167 branched at `c046a7d` (#169, media-refresh) — the same base #161 had, and
by now a long way back. Main had taken **127 commits** since, including the screen
lock (#154), the state custodian (#157, #172), the packaged custodian commands,
web management authentication, network status, and multiple companion bonds
(#149). This note records the rebase onto `69f4b16` and the re-run of every gate,
plus the one real defect re-verification turned up.

## The rebase

All 12 commits replayed onto `69f4b16` with **no conflicts**, and the branch's own
patch was diffed before vs. after: identical file set and identical line counts.
Nothing was re-merged by hand.

A clean replay is worth exactly nothing here, though, and this branch is the case
that shows why. Its whole surface is the build and packaging path — `build.sh`,
`package.sh`, `deps.sh`, the workflows — and main had edited three of those four
files underneath it. Git merged them because the edits never touched the same
lines. Whether the *product* still works is a separate question, and only running
it answers that.

## What main changed underneath it

Main added three binaries the cross path had never seen: `shepherd-lock` (#154),
`shepherd-validate-config` (#157) and `shepherd-stated` (#172). The first two went
into `SHEPHERD_BINARIES`, which `build.sh` already treats as the single list every
consumer reads; the third is staged directly by `install.sh` from
`get_target_dir`. All three are workspace `default-members`.

That is why the rebase held: the cross work hooks `get_target_dir` and
`SHEPHERD_BINARIES` rather than enumerating binaries of its own, so three binaries
that did not exist when it was written cross-compile and package with no change.
Confirmed rather than assumed — all twelve binaries in the arm64 build are
`ELF 64-bit LSB pie executable, ARM aarch64`, interpreter
`/lib/ld-linux-aarch64.so.1`.

No new system library came with them: `shepherd-lock` needs cairo and wayland,
both already in `cross.pkgs`, and `shepherd-stated` is pure Rust plus zbus.
`build.pkgs` did not change on main at all, which is also why
`check-cross-pkgs.sh` passing is weak evidence rather than strong — it had nothing
to catch this time.

## The defect: the mirror probe could not answer for a PPA

`deps install cross --arch arm64` would, on this host, have rewritten every apt
source to `Architectures: amd64` and added a redundant ports entry. Two bugs
compounded, both in the "does the configured mirror serve this architecture?"
path:

1. `_deps_enable_foreign_arch` read the first `URIs:`/`Suites:` line across
   `/etc/apt/sources.list.d/*.sources` — that is whichever file *sorts first*.
   Here that is `libretro-ubuntu-testing-resolute.sources`, a PPA, not
   `ubuntu.sources`. Every conclusion about "the configured mirror" was drawn
   from a PPA.
2. `_deps_mirror_serves` probed a hardcoded `main universe`. A Launchpad PPA only
   ever publishes `main`, so the probe 404ed on a component the stanza never
   claimed to have and returned "does not serve this architecture" — for a PPA
   whose Release file lists `arm64` explicitly.

The second is the load-bearing one: it makes the probe **unanswerable for any
PPA**, not just a mis-ordered one.

The consequence landed exactly where it hurts this project. `ppa:libretro/testing`
is Libretro's own PPA (`~libretro` on Launchpad is the Libretro team) and it
publishes `arm64` for 119 packages — `libretro-2048`, `libretro-a5200`,
`libretro-anarch`, … Pinning it to `Architectures: amd64` would make every one of
those emulation cores invisible to apt, on a device whose primary use is
supervised emulation. All three Libretro PPAs (`stable`, `testing`, `extra`) carry
the same set:

```
Architectures: amd64 amd64v3 arm64 armhf i386 ppc64el riscv64 s390x
Components: main
```

So there was nothing to switch to — the configured source was already the right
one, and the script was wrong about it.

### The fix

Probe **per stanza, against the components that stanza declares**:

* `_deps_source_stanzas` parses deb822 into one record per stanza. Per stanza,
  not per file, because `ubuntu.sources` alone holds two (archive and security).
* `_deps_mirror_serves` takes the components as an argument. *All* of them must
  serve the architecture — `apt-get update` fetches an index per
  component × architecture and fails hard on a 404 — but "all" now means all the
  stanza actually has.
* Pinning is decided per source, so a source that serves the target architecture
  keeps serving it. The ports fallback keys off the Ubuntu-archive-shaped stanza
  (`main` *and* `universe`), because the `-dev` packages a cross build links
  against come from the archive; a PPA serving the architecture is not a
  substitute for the archive doing so.

Verified both directions on this host, against the live mirrors:

| Source | arm64 verdict | riscv64 verdict (fallback path) |
| --- | --- | --- |
| `libretro…testing.sources` (PPA, `main`) | serves → left alone | serves → left alone |
| `ubuntu.sources` (archive + security) | serves → left alone | does not → pinned **once** |
| `vscode.sources` | already declares `Architectures:` → skipped | skipped |
| ports entry | not needed | added |

And on the real run, `/etc/apt/sources.list.d/` came out **byte-identical**
(md5sums unchanged, no `.pre-cross` backups, no `ubuntu-ports-arm64.sources`);
the only change to the host was `dpkg --add-architecture arm64`. `apt-get update`
then fetched arm64 indexes from both the archive and the Libretro PPA.

## Gates (post-rebase)

| Gate | Result |
| --- | --- |
| `cargo test --workspace --all-targets` | 1386 passed, 0 failed, 26 ignored (69 suites) |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo fmt --all -- --check` | clean |
| `shellcheck` (as CI runs it) | clean |
| `scripts/ci/check-cross-pkgs.sh` | OK |
| `scripts/ci/check-arch-neutral.sh` | OK |
| `scripts/ci/check-workflows.sh` | OK (no duplicate job names) |
| `shepherd version check` | 0.5.1 everywhere |
| `shepherd-validate-config config.example.toml` | passes |
| `npm test` / `typecheck` / `check:boundary` / `check:coverage` | 120 passed; clean; clean; 145/145 fields reachable |

## Gates that needed the cross toolchain

`deps install cross --arch arm64` → `build --arch arm64` → `package deb --arch
arm64`, run natively on this amd64 host — the same direction CI takes, and the
mirror of the arm64→amd64 proof in `2026-09-04 002`.

* All 12 binaries are `ARM aarch64`, interpreter `/lib/ld-linux-aarch64.so.1`.
  That includes `shepherd-lock`, `shepherd-validate-config` and `shepherd-stated`,
  none of which existed when this branch was written.
* The eBPF object stayed `ELF 64-bit LSB relocatable, eBPF`, with **no** host- or
  target-triple directory beside it in the BPF crate's `target/`.
* The `.deb` declares `Architecture: arm64`, and **every** ELF inside it is
  `ARM aarch64` — all 12, including `/usr/libexec/shepherd-firewall-helper` and
  `/usr/libexec/shepherd-stated` — with no x86-64 file anywhere. Its file set is
  identical to the amd64 package's, so label and contents agree.

### The eBPF guard, tested in both directions

`crates/shepherd-firewall-helper/build.rs` strips `CARGO_BUILD_TARGET` before
shelling out to the BPF crate. That it is *load-bearing* was confirmed by removing
the line: with `CARGO_BUILD_TARGET=x86_64-unknown-linux-gnu` exported the BPF build
then failed at link — `rust-lld: error: undefined symbol: __libc_start_main`,
against `Scrt1.o` — because the eBPF program was being compiled for the host
triple. Restored, the same build keeps the object as eBPF. The host triple is
enough to demonstrate this; no cross toolchain is involved.

(Removing the guard also leaves an `x86_64-unknown-linux-gnu/` directory inside
`crates/shepherd-firewall-bpf/target/`. If one appears there, that is the
signature — CI asserts only that the object is eBPF.)

### Packaging, host architecture

`package deb` with no `--arch` still produces `shepherd-launcher_0.5.1_amd64.deb`
with all ten `SHEPHERD_BINARIES` staged, and `package deb --arch amd64` on an
amd64 host produces **byte-identical contents** — the no-op path the CI matrix
depends on, so passing `--arch` on every leg costs nothing.

## Left open

**`--arch armhf` silently targets the wrong ISA.** `arch_to_triple` derives
`arm-unknown-linux-gnueabihf` from `dpkg-architecture`, and that triple *is* in
`rustc --print target-list`, so the guard passes. But Debian's `armhf` is ARMv7
hard-float; `arm-unknown-linux-gnueabihf` is the ARMv6 baseline, and the ARMv7
triple is `armv7-unknown-linux-gnueabihf`. The comment above the function claims
armhf "fails here rather than at link time" — it does not. The result would run
(ARMv6 code runs on ARMv7) while being labelled Debian `armhf`, so this is a
silent pessimisation rather than a break. Not fixed here: `arm64` is the only
architecture this PR claims, and choosing between rejecting `armhf` and mapping it
to the ARMv7 triple is a design decision, not a rebase repair.

**One-line (`.list`) apt sources are not covered.** `_deps_enable_foreign_arch`
only parses deb822 `*.sources`. This host also has `waydroid.list`, which has no
`[arch=…]` restriction; nothing broke, but a one-line source that cannot serve the
new architecture would not be pinned.

# Launchpad PPA feasibility (#204)

## Prompt

> `/remote-control` — investigate the feasibility of #204

Investigation only: nothing in the tree was changed. Every probe below ran in
the session scratchpad against the working tree at `847c1f4` (0.5.1), on this
Ubuntu 26.04 dev box (8 vCPU, 11 GB RAM).

## Verdict

**Technically feasible. Every piece that looked like a blocker in the issue
turned out to be surmountable: the whole workspace — including the eBPF object
the issue singles out — builds offline against Ubuntu's own archive toolchain,
and the resulting firewall passes `firewall_cgroup` on this kernel.** What it
costs is a second,
parallel build system — a `debian/` tree, a vendored source tarball, and a
patched eBPF build path — maintained next to the one CI already has, in
exchange for benefits the project mostly already owns (arm64, which the cross
image already builds) or can get far more cheaply (signed repository metadata,
which any self-hosted apt repo gets with the key #203 needs anyway).

If the goal is "`apt install lunchbox` + `apt upgrade`, signed, on 26.04", a
self-hosted apt repo generated from the `.deb` CI already builds is a much
smaller delta. The PPA's one structural advantage is that Launchpad hosts,
signs and builds natively on arm64 for free — and the price is that it will
only ever build what fits inside its builders.

## What a PPA actually demands

| Constraint | Consequence here |
|---|---|
| Source uploads only; binary uploads are rejected | The CI-built `.deb` cannot be published. Launchpad rebuilds it from a `.dsc` on its own builders. |
| Builders have **no internet** ([manual](https://documentation.ubuntu.com/launchpad/developer/explanation/launchpad-ppa/)) | Every crates.io crate, every npm package, and the `-Zbuild-std` sysroot crates must ship inside the source package or come from the Ubuntu archive. |
| Build-deps come from the PPA + **all components** of the primary archive | `rust-src`, `llvm-21-dev` (universe), `nodejs`/`npm` are all reachable. `bpf-linker` and `wasm-pack` are not packaged at all and must be built in-tree. |
| 3-hour build timeout | Not a problem: see the timing below. |
| 8 GiB default quota, larger on request | ~95 MB of vendored orig tarball per version; fine for dozens of releases. |
| `amd64` builds by default; other architectures are a checkbox | arm64 is a per-PPA "Processors" setting, and then builds natively — no cross image, no sysroot. |
| Uploads are GPG-signed with a key registered on the Launchpad account | Another key-in-CI question, adjacent to #203 but not the same key (Launchpad signs the repository metadata itself). |
| One `debian/changelog` entry per release per series | Lunchbox targets 26.04+ only, so this is one series — the Launchpad series matrix buys nothing here. |

## What the build needs today, and where each piece lands on a builder

`./scripts/lunchbox package deb` today assumes a host provisioned by
`lunchbox deps install`, which is a much richer environment than a builder:

| Need | Provided today by | On a Launchpad builder |
|---|---|---|
| 646 crates.io deps (`Cargo.lock`: 669 packages) | `cargo fetch` over the network | must be vendored into the orig tarball |
| Rust toolchain | rustup stable (1.98 here) | `rustc`/`cargo` 1.93.1 from the archive — **works** (probe 2) |
| eBPF crate: nightly + `rust-src` + `-Zbuild-std` | `rustup run nightly` in `lunchbox-firewall-helper/build.rs` | `rust-src` + `RUSTC_BOOTSTRAP=1` — **works** (probes 1, 3) |
| `bpf-linker`, pinned 0.10.3 | `cargo install` over the network | build from vendored source against `llvm-21-dev` — **works** (probe 4) |
| `-Zbuild-std` sysroot deps | fetched implicitly | a **separate vendoring pass**, pinned to the archive rustc (probe 5) |
| Web UI: npm + 240 packages, rsbuild | `npm install` over the network | `npm ci --offline` from a shipped cache — works, but see probe 6 |
| Config editor wasm: `wasm-pack` + `wasm32-unknown-unknown` | `cargo install wasm-pack`, `rustup target add` | neither is in the archive — build in-tree or ship the artifact |

## Probes, and what they found

### 1. The eBPF crate does not actually need nightly

`crates/lunchbox-firewall-bpf` pins `channel = "nightly"` and the helper's
`build.rs` shells out to `rustup run nightly`. But what it uses nightly *for*
is `-Zbuild-std=core`, and that is reachable on a stable compiler with
`RUSTC_BOOTSTRAP=1` plus the `rust-src` component:

```
RUSTC_BOOTSTRAP=1 rustup run stable cargo build --release
  → Finished `release` profile in 5.99s
  → ELF 64-bit LSB relocatable, eBPF
```

That matters because Ubuntu ships `rust-src` (1.93.1) but will never ship a
nightly toolchain.

### 2. The workspace compiles with Ubuntu's archive toolchain, offline, from a vendored tree

Installed `rustc`/`cargo`/`rust-src` 1.93.1 from the archive, pointed
`CARGO_HOME` at a config whose only source is `vendor/`, and built with
`--offline --locked`:

```
/usr/bin/cargo build --release --offline --locked
  → Finished `release` profile [optimized] target(s)
  → all of LUNCHBOX_BINARIES present, lunchboxd 33.8 MB
```

No crate needed a newer compiler than the archive's, and nothing reached the
network. Cost: **~1080 s of CPU (≈18 CPU-minutes), 2m30s wall on 8 cores, 1.6
GB peak RSS**. A Launchpad builder is slower, but this is comfortably inside
the 3-hour timeout.

Caveat: the probe used the `lunchbox-webui/dist` already present in the tree,
so it did not exercise the npm/wasm half — that is probe 6.

### 3. …including the eBPF object, with no rustup anywhere

`build.rs` hardcodes `rustup run nightly cargo`, so the probe put a shim named
`rustup` on `PATH` that re-execs `/usr/bin/cargo` with `RUSTC_BOOTSTRAP=1` —
which is exactly the patch a `debian/rules` would have to carry. With that, the
archive cargo built the BPF object (3160 bytes) and the helper embedded it.

### 4. `bpf-linker` builds from source against the archive's LLVM

`bpf-linker` is not packaged for Ubuntu, and the pinned 0.10.3 defaults to
`rust-llvm-22`, which links through rustc's *bundled* LLVM — a rustup-ism. Its
`llvm-21` feature links the system LLVM instead, and Ubuntu 26.04 has
`llvm-21-dev` (universe), matching the archive rustc's own LLVM 21:

```
cargo install bpf-linker --version 0.10.3 \
  --no-default-features --features llvm-21 --locked
  → needs llvm-config on PATH (/usr/lib/llvm-21/bin) or LLVM_SYS_211_PREFIX
  → bpf-linker 0.10.3, 47 MB
```

So the LLVM coupling `scripts/lib/deps.sh` warns about is satisfiable from the
archive — *if* the archive rustc and the chosen `llvm-NN` feature stay in step.
When Ubuntu moves rustc to a new LLVM, that feature has to move with it, and
the failure mode deps.sh names is a BPF object the verifier rejects at runtime,
not a build error.

That runtime failure mode is the one worth checking rather than assuming, so I
checked it. `bpftool prog load` cannot: aya emits legacy `maps`-section
definitions that libbpf v1.0+ refuses, and aya's own loader is what handles
them. The repository's hermetic root-only test does exactly the right thing
instead — it loads the embedded object through the helper, attaches it to a
cgroup made for the occasion, and probes an allowed and a denied address:

```
sudo cargo test --release -p lunchbox-e2e --test firewall_cgroup \
  --offline --locked -- --include-ignored
  → [OK] cgroup firewall filtered as configured
         (allow=127.0.0.1:37883, deny=192.168.122.130:36585)
  → test result: ok. 1 passed
```

So the object built by rustc 1.93 + a system-LLVM-21 `bpf-linker` loads,
verifies and filters on 7.0.0-31-generic. The LLVM coupling is a live risk for
*future* archive updates, not a blocker today.

### 5. `-Zbuild-std` needs its own vendoring pass, pinned to the archive rustc

The first offline build got all the way to `lunchbox-firewall-helper` and died:

```
error: no matching package named `rustc-literal-escaper` found
  required by package `proc_macro v0.0.0`
      (/usr/lib/rust-1.93/lib/rustlib/src/rust/library/proc_macro)
```

`cargo vendor` over the workspace does not capture what `-Zbuild-std` needs to
compile `core`. Fixing it takes a second sync against the sysroot manifest —
itself requiring `RUSTC_BOOTSTRAP=1`, because the sysroot manifest uses the
nightly-only `public-dependency` feature:

```
RUSTC_BOOTSTRAP=1 cargo vendor --versioned-dirs \
  --sync crates/lunchbox-firewall-bpf/Cargo.toml \
  --sync crates/lunchbox-config-wasm/Cargo.toml \
  --sync /usr/lib/rust-1.93/lib/rustlib/src/rust/library/sysroot/Cargo.toml \
  vendor/
```

Note *which* version it vendored: `rustc-literal-escaper 0.0.5` for the
archive's 1.93 sysroot, where rustup's 1.98 sysroot wanted 0.0.8. **The source
package is therefore pinned to the Ubuntu rustc it was vendored against.** An
SRU that bumps rustc in 26.04 can break a PPA rebuild of an already-published
source package — the one maintenance hazard here that has no clean answer
short of not building the BPF object on Launchpad at all.

### 6. The web UI is the least "source" part of the source package

`npm ci --offline` works from a populated cache — 240 packages in 7 s, and the
cache for exactly this lockfile is **78 MB**. But the packages it installs
include `@rspack/binding-linux-x64-gnu` and friends: **prebuilt native
binaries**, selected by `cpu`/`os`, one per platform. An amd64-populated cache
does not contain the arm64 binding, so a native arm64 builder would need its
own cache — and either way the "source" package would be shipping precompiled
blobs it then runs.

`wasm-pack` compounds it: not in the archive, and neither is a `wasm32-unknown-unknown`
std (it would have to come from `-Zbuild-std` too).

The honest answer is to **not build the web UI on the builder**: ship the
already-built `lunchbox-webui/dist` (3.3 MB) and `src/config/wasm` (1.0 MB) in
the orig tarball, built by the existing CI job. A PPA does not review sources,
so this is allowed; it is also exactly the thing that would disqualify the
package from the Ubuntu archive proper, if that were ever a goal.

### 7. Sizes

| Artifact | Size |
|---|---|
| `cargo vendor` tree (646 crates, +13 sysroot) | 849 MB |
| …minus `windows-*`/`web-sys`/`js-sys` (a `cargo-vendor-filterer` pass) | 396 MB |
| …as `.tar.xz` | **94 MiB** |
| npm cache for the web UI lockfile | 78 MB |
| `node_modules`, if shipped instead | 459 MB |
| Built `dist/` + wasm, if shipped instead of either | 4.3 MB |
| Current `lunchbox_0.5.1_amd64.deb` | 21 MB |

## The work it implies

1. **A `debian/` tree.** `control`, `rules`, `changelog`, `source/format`, the
   three maintainer scripts. Not a rewrite of the layout: `install.sh` is
   already `DESTDIR`-aware, so `override_dh_auto_install` can call
   `install_system` exactly as `package_deb` does today, and the single-sourced
   layout survives. The parts that need care are the ones `package.sh`
   *generates*: `Depends` is derived from `scripts/deps/run.pkgs`, and the
   maintainer scripts interpolate constants from `install.sh`
   (`FIREWALL_GROUP`, `STATED_USER`, `BLUETOOTH_DROPIN_DIR`, …). Either the
   release job generates `debian/` into the tarball (keeping the single
   source), or those values are duplicated statically and drift.
2. **A source-package job** that vendors crates + sysroot deps, drops in the
   prebuilt web assets, writes `debian/changelog` for the series, builds the
   `.dsc`, signs it and `dput`s it.
3. **A build-path patch for the firewall crate**, so `build.rs` uses
   `RUSTC_BOOTSTRAP=1` + archive cargo instead of `rustup run nightly`, and so
   `bpf-linker` is built in-tree with the `llvm-NN` feature matching the
   archive rustc.
4. **An upload key** on the Launchpad account, and a decision about whether it
   lives in CI (see #203, which is about a different key but the same question).
5. **A verification story for the eBPF object**, since the PPA build uses a
   different LLVM than the one CI validates against. `firewall_cgroup` is the
   test that covers it, and it cannot run on a Launchpad builder.

## What it buys, and what it doesn't

**Buys:** hosting and bandwidth; repository metadata signed by Launchpad
(answering half of #203 for free); native arm64 builds; the familiar
`add-apt-repository ppa:…` path; the build proven to work from source on a
stock Ubuntu.

**Doesn't buy:** relief from the arm64 cross setup — the cross image and the
`Build (arm64 cross)` CI job already work and would stay as the PR gate;
multi-series coverage, since only 26.04+ is supported; and it actively costs
the ability to publish *the artifact CI tested*, because Launchpad rebuilds
from source on its own builders with its own toolchain.

## Cheaper alternatives that reach the same user-visible outcome

1. **Self-hosted apt repo** (`aptly`/`reprepro`, or any static-file host) fed
   by the `.deb` CI already builds. Keeps `docs/INSTALL.md`'s existing
   instructions — they already describe `apt.lunchbox-os.com` with a
   `signed-by` keyring — publishes the tested binary, needs no vendoring, no
   `debian/` tree, no eBPF toolchain contortions, and needs exactly the signing
   key #203 already has to decide on. This is the smallest delta from where the
   project is now.
2. **GitHub Releases only**, with `.deb` assets and no `apt upgrade` path. What
   the repo does today minus the registry; a regression for users.

## Recommendation

Treat #204 as **possible but not the default**. Do #203 first — a signing key
is needed under either plan — then stand up the self-hosted repo from the
existing `.deb`. Revisit the PPA if the motivation turns out to be "be where
Ubuntu users look" or "stop maintaining a repo host" rather than "replace the
Forgejo registry", because those are the two things it genuinely does better.

If it is pursued anyway, the order that de-risks it fastest is: (a) confirm the
archive-toolchain BPF object passes `firewall_cgroup` on the target kernel,
(b) land the `debian/` tree and build it locally with `sbuild`/`pbuilder` in a
26.04 chroot with networking off, (c) only then wire up the upload.

## Probe scripts

Kept out of the tree; they lived in the session scratchpad:
`archive-toolchain-build.sh` (vendored offline build), `archive-build-2.sh`
(same, with the `rustup` shim and the locally built `bpf-linker`),
`firewall-cgroup-probe.sh` (the eBPF runtime check). All three are short and
reconstructible from the commands quoted above.

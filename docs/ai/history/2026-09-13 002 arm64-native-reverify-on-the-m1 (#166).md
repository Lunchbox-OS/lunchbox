# Re-verifying #167 natively on the M1 VM (issue #166)

Prompt: "#167 is checked out, and you're back on the M1 machine. Reverify,
including the more touchy ARM-specific setup behavior like retroarch, okular,
and shepherd-media."

The previous pass
([`2026-09-13 001`](./2026-09-13%20001%20arm64-cross-rebase-reverify%20(#166).md))
rebased the branch onto main and re-ran every gate on the **amd64** host,
cross-compiling towards arm64. This one re-runs them on the **arm64** host, at
`fe39b8b`, and adds the half no cross build can reach: the activity backends an
operator actually installs on an ARM device.

Host: Ubuntu 26.04.1, `aarch64`, kernel **7.0.0-31**-generic (the earlier native
pass, `2026-09-04 002`, ran on -30).

## tl;dr

Everything passes natively, and the three commits added since the last native
pass — the uninstall guard, the per-stanza mirror probe, and the armhf triple —
behave correctly *on this host*, which is where two of them were found.
RetroArch (archive **and** libretro PPA), Okular and `shepherd-media` were
installed and driven end to end on arm64 for the first time, and the ports
fallback — recorded in `2026-09-04 002` as exercised nowhere — finally ran.

Two defects surfaced and are fixed here: a **flaky e2e test** (unrelated to
the branch) and **`armel` deriving the ARMv6 triple**, the same bug `fe39b8b`
fixed for armhf.

## Gates

| Gate | Result |
| --- | --- |
| `cargo test --workspace --all-targets` | 1386 passed, 0 failed, 26 ignored (69 suites) |
| `cargo test -p shepherd-e2e -- --include-ignored` (`SHEPHERD_REQUIRE_PEER_CGROUP=1`) | 19 passed, 0 failed, **0 ignored** (13 suites) |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo fmt --all -- --check` | clean |
| `shellcheck` (as CI runs it) | clean |
| `check-cross-pkgs.sh` / `check-arch-neutral.sh` / `check-workflows.sh` | OK |
| `shepherd version check` | 0.5.1 everywhere |
| `shepherd-validate-config config.example.toml` | passes |
| `npm test` / `typecheck` / `check:boundary` / `check:coverage` | 120 passed; clean; clean; 145/145 fields reachable |

Identical counts to the amd64 run, which is the point: nothing in this workspace
is architecture-conditional.

### The firewall BPF suites, on kernel 7.0.0-31

The reason this VM exists. All three re-run green on the newer kernel:

- `firewall_cgroup` as root with `SHEPHERD_FIREWALL_CGROUP_REQUIRED=1` —
  `cgroup firewall filtered as configured (allow=127.0.0.1, deny=192.168.64.12)`.
- The same, with `/sys/fs/cgroup` forced read-only under
  `unshare -m --propagation private`, exactly as `ci.yml` does it. The log proves
  it took the fallback (`creating cgroups through a private cgroup2 mount at …`),
  not a writable cgroupfs.
- `firewall_real` as the non-root `tester` in `shepherd-firewall`: polkit,
  installed helper, `systemd-run` scope, real filtering — `allow=OPEN`,
  `deny=BLOCKED`.

`shepherd install firewall` also migrated the polkit rule from
`/etc/polkit-1/rules.d/` to `/usr/share/polkit-1/rules.d/` and removed the
superseded copy, unprompted — a main-side change this VM had not seen.

## Native build and package

`shepherd build` → `package deb`: `shepherd-launcher_0.5.1_arm64.deb`,
`control.tar.xz` + `data.tar.xz`, **12 ELF binaries, every one `ARM aarch64`**,
no x86-64 file anywhere. That includes `shepherd-lock`,
`shepherd-validate-config` and `shepherd-stated`, which did not exist when this
branch was written.

## The three post-rebase commits, re-checked where they were found

### `bdb539e` — refusing a cross install that would uninstall the native one

This host is the one that exposed the problem. `deps install cross --arch amd64`
now refuses, naming the packages:

```
[WARN] This would REMOVE these natively-installed packages:
[WARN]   libarchive-dev libcdio-cdda-dev libcdio-dev libcdio-paranoia-dev
[WARN]   libext2fs-dev libgirepository1.0-dev libmpv-dev
[ERROR] Refusing: that would break the native build on this host …
```

### `4601aa5` — probing each stanza's own components

The fix's load-bearing half is the **per-stanza** split, and this host shows it
directly: `ubuntu.sources` holds two stanzas and both are now probed on their
own, where the old code read the first `URIs:`/`Suites:` line in the file and
drew every conclusion from it.

```
ubuntu.sources  resolute           amd64 SERVES   riscv64 does NOT   arm64 SERVES
ubuntu.sources  resolute-security  amd64 SERVES   riscv64 does NOT   arm64 SERVES
ports                              amd64 does NOT riscv64 SERVES     arm64 SERVES
```

After a real `deps install cross --arch amd64`, `/etc/apt/sources.list.d/` was
**byte-identical** (md5sums unchanged, no `.pre-cross` backups, no ports entry,
no `Architectures:` pin). The only host change was `dpkg --add-architecture`.

#### With the libretro PPA added, which is the shape the bug was found in

The first draft of this note said the PPA half "cannot be re-demonstrated here —
this host has no PPA". That is a reason to add one, not to skip it. Installing a
PPA-only core (`apps install retroarch --ppa mupen64plus-next`, below) puts this
host in exactly the configuration that broke the old probe, and both halves of
the bug reproduce:

* **The glob order.** `libretro-ubuntu-testing-resolute.sources` sorts *before*
  `ubuntu.sources`, so the old "first `URIs:`/`Suites:` line across the glob"
  read a PPA and drew every conclusion about "the configured mirror" from it.
* **The hardcoded components.** Probed by hand against the live PPA:

  ```
  main universe -> does NOT serve   <- the old, unanswerable verdict
  main          -> SERVES           <- the truth
  ```

The stanza parser now yields three records — the PPA, the archive, and security —
and each is probed against its own components.

**The pinning path, run for real.** `--arch riscv64` is the case that actually
rewrites something on this host, so it exercises the ports fallback that
`2026-09-04 002` recorded as "exercised nowhere — not here, not in CI":

```
[INFO] …/libretro/testing/ubuntu/ (resolute) serves riscv64; leaving …libretro….sources alone
[INFO] http://us.archive.ubuntu.com/ubuntu/ (resolute) does not serve riscv64; ubuntu.sources will be pinned
[INFO] http://security.ubuntu.com/ubuntu/ (resolute-security) does not serve riscv64; ubuntu.sources will be pinned
[INFO] Pinning /etc/apt/sources.list.d/ubuntu.sources to arm64...
[INFO] Adding a http://ports.ubuntu.com/ubuntu-ports/ entry for riscv64...
```

Afterwards: the **PPA file is byte-identical** (md5 unchanged, no `Architectures:`
line added), `ubuntu.sources` carries `Architectures: arm64` once per stanza —
both of them, which is what `apt-get update` needs — the ports entry names
`Architectures: riscv64` across all four suites, `apt-get update` succeeded, and
all **102** libretro packages stayed resolvable. The `.pre-cross` backup was
byte-identical to the original, and restoring from it returned
`/etc/apt/sources.list.d/` to its pre-run md5sums exactly.

Three things that run only turned up:

1. The riscv64 *install* then failed (`EXIT=100`), and correctly: with amd64
   already enabled, `libc6:amd64 Breaks libc6:riscv64`. A genuine three-way
   multiarch conflict at the apt layer, not a script fault — and moot anyway,
   since `arch_to_triple` refuses riscv64 before a build could start.
2. The uninstall guard ran first and found nothing to remove, so an
   *unsatisfiable* set is not something it covers. It answers "what would this
   uninstall", not "will this resolve at all"; apt reports the latter.
3. Pinning **reflows a stanza's comments**. `ubuntu.sources` has a commented-out
   alternate mirror (`# URIs: http://mirrors.mit.edu/ubuntu/`) directly under the
   active `URIs:`; after pinning it sits at the end of the stanza instead.
   Semantically identical — same stanza, no blank line introduced — but the
   comment is no longer next to the line it annotates. Cosmetic, unreported here
   as anything more.

### `fe39b8b` — armhf is ARMv7, not ARMv6 — and armel has the same bug

Checked on the machine whose family armhf belongs to. The armhf override holds,
and checking it turned up **a second architecture with the identical defect**:
`armel`. `dpkg-architecture` reports `arm-linux-gnueabi`, which derives
`arm-unknown-linux-gnueabi` — a real Rust target, so the target-list guard
accepts it. But Rust's bare `arm-*` triples are the **ARMv6** baseline
(`features: +strict-align,+v6`, read from `--print target-spec-json`), while
Debian defines armel as **ARMv5TE soft-float**, which Rust spells
`armv5te-unknown-linux-gnueabi`.

armel's failure mode is worse than armhf's, not better: ARMv6 code *runs* on an
ARMv7 machine, so armhf was quietly wrong, but ARMv6 instructions *fault* on
ARMv5TE hardware — loudly, at the far end, long after the package looked fine on
the build host. Added as the second `ARCH_TRIPLE_OVERRIDES` entry, on exactly the
reasoning `fe39b8b` gave for the first.

| Debian arch | GNU type | Rust triple |
| --- | --- | --- |
| `arm64` | `aarch64-linux-gnu` | `aarch64-unknown-linux-gnu` |
| `amd64` | `x86_64-linux-gnu` | `x86_64-unknown-linux-gnu` |
| **`armhf`** | `arm-linux-gnueabihf` | **`armv7-unknown-linux-gnueabihf`** (overridden) |
| **`armel`** | `arm-linux-gnueabi` | **`armv5te-unknown-linux-gnueabi`** (overridden, new) |
| `i386`, `ppc64el`, `s390x`, `loong64` | — | derive unchanged |
| `riscv64` | `riscv64-linux-gnu` | refused, loudly, as designed |

Neither armhf nor armel is an architecture this PR claims to support. Both are
corrected because a wrong answer that passes the guard is worse than a refusal.

## The cross link, mirror direction, with all twelve binaries

`deps install cross --arch amd64 --allow-remove` → `build --arch amd64` →
`package deb --arch amd64`, natively on arm64:

- **All 12 binaries linked and are x86-64.** The last native pass proved nine.
- The eBPF object stayed `ELF … eBPF`, and `shepherd-firewall-bpf/target/`
  grew no host- or target-triple directory.
- `shepherd-launcher_0.5.1_amd64.deb`: `Architecture: amd64`, every ELF inside
  x86-64, zero aarch64 files, and a **file set identical to the arm64 package's**
  — label and contents agree in both directions.

`deps install build` restored the native set afterwards, and a plain
`shepherd build` produces `ARM aarch64` binaries again. `deps check dev` passes.

## The ARM-specific setup behavior

This is what the M1 VM buys that a cross build cannot, and none of it had been
exercised on arm64 before.

### RetroArch

All 14 archive cores in `RETROARCH_CORES` have arm64 candidates in
`resolute/universe`, so the table needs no per-architecture handling.
`sudo shepherd-admin apps install retroarch` installed `retroarch` 1.22.2,
`retroarch-assets`, `libretro-core-info` and `libretro-mgba`, all aarch64.

**The PPA path works on arm64 too.** All three libretro PPAs advertise
`Architectures: amd64 amd64v3 arm64 armhf i386 ppc64el riscv64 s390x` with
`Components: main` for `resolute`. `apps install retroarch --ppa mupen64plus-next`
added `ppa:libretro/testing` and installed the N64 core: visible `libretro-*`
packages went from **16 to 102**, and all 102 have an arm64 candidate — so the
"PPA adds cores" claim in `docs/emulators.md` holds on this architecture, not
just amd64.

That core is also the naming-mismatch case: `core = "mupen64plus-next"` resolved
against the real file on disk to
`/usr/lib/aarch64-linux-gnu/libretro/mupen64plus_next_libretro.so`, with no
diagnostic and the entry enabled. Package name, config spelling and shared-object
name all disagree, and the resolution handles it on arm64 as it does elsewhere.

The core landed at `/usr/lib/aarch64-linux-gnu/libretro/mgba_libretro.so`, and
`core_search_dirs()` found it by reading `/usr/lib` rather than guessing a
triple — the shepherdd log shows the resolved absolute path in the launch argv.
Launched from the grid with the MIT-licensed `jsmolka/gba-tests` ROM the
emulator docs recommend, RetroArch mapped as `com.libretro.RetroArch`, drew
"Failed test 235" (the ROM grading the core, as documented), and the HUD showed
the session with its reset control. Closing it wrote
`states/mGBA/gba-tests-arm.state.auto` — save-on-close works here too.

### Okular

`sudo shepherd-admin apps install okular` installed Okular 25.12.3 and the
backends. The one that matters is present for arm64:
`/usr/lib/aarch64-linux-gnu/qt6/plugins/okular_generators/okularGenerator_epub.so`.
Launching the `ebook` entry against a Project Gutenberg EPUB brought up
`org.kde.okular` rendering the book, with shepherd's generated kiosk
configuration and the HUD's page-turn controls.

Two things this turned up that are not defects but are worth knowing:

- The `ebook` entry is gated by `ProtectionUnavailable` unless the session user
  is in `shepherd-firewall`; `config.example.toml` puts a firewall on it. On a
  dev box that means `usermod -aG shepherd-firewall` and a session restarted
  with that group.
- Installing Okular pulls in Breeze, which **changes the launcher's fallback
  icons** for every entry with no icon of its own. Cosmetic, but it makes
  before/after screenshots of the grid differ for a reason unrelated to the
  change under test.

### shepherd-media

- `shepherd-admin va-api detect` is correct on this host and takes the branch
  written for ARM boards: no PCI display controller of the usual vendors, so it
  reports `other (0x1af4)` (virtio-gpu) and *"Nothing to install: this hardware
  is served by the drivers Mesa ships, or its drivers are not packaged for this
  architecture"*. No package is guessed at, which is the intended behavior.
- Playback works: `shepherd-media` decoded and displayed an H.264 clip, and the
  browse view rendered the library. `yt-dlp` runs (the example's placeholder
  playlist id produces the expected `MediaLibraryUnreadable` diagnostic).
- `libmpv_backend` correctly warns *"mpv is decoding video in software"* — this
  VM has no VA-API driver and the headless session forces software rendering
  anyway. **Hardware decode on real ARM silicon is still untested**, exactly as
  `2026-09-04 002` left it.

## The defect this pass found: a flaky e2e test

`firewall_supported_path_invokes_helper_with_expected_argv` failed once, then
passed 21 consecutive re-runs including 15 under full CPU load:

```
expected argv element "--gid" in helper log; got:
ARGV_BEGIN / apply-process / --scope-name / <scope> / --uid
```

The log is truncated mid-argv, with no `ARGV_END`. `wait_for_file` waits for the
file to **exist**, and a shell redirection creates it before a byte is in it —
then `/bin/sh` (dash) flushes stdout after each command, so the log grows a few
lines at a time and a reader that only waited for existence can catch a prefix.
Demonstrated directly: reading the same stub's output mid-run yields exactly
that shape of partial file. The assertion then blames a flag that is merely not
written yet.

Fixed by having both argv-recording stubs write to `<log>.partial` and `mv` it
into place. Rename is atomic, so "the file exists" and "the file is complete"
become the same statement and `wait_for_file` is honest again. `browser.rs` had
the identical latent race (single `printf`, but the file still exists before the
write lands) and got the same treatment.

Test-only. It is unrelated to #166 and rides along because a gate that fails
once in a while is not a gate.

## Observations, not acted on

- **`write_policy_file` is not atomic.** `std::fs::write` creates the Chrome
  managed-policy JSON empty and then fills it, so a reader — including Chrome —
  can observe a partial policy. Product code, out of scope here; worth an issue.
- **`VA_DRIVER_DIRS` globs `/usr/lib/*/dri`, which is architecture-blind.** On a
  multiarch host (this one, after a cross install) it matches the foreign
  architecture's Mesa too, so `va-api detect` could report a driver "present"
  that the native libva cannot load. Harmless here — both directories carry the
  same driver names — and a multiarch host is not what a device looks like.
- **The software-decode warning and `va-api detect` disagree in tone.** The
  warning suggests installing `va-driver-all`; the admin tool says nothing
  applies to this hardware. Both are right for their own question, but an
  operator reading the first would go looking for a package the second declined
  to name.
- **`dev key` drops repeated keysyms.** `dev key Down Down Right Right Right`
  moved the grid selection three steps, not five, so the wrong entry launched
  twice. Driving the management API (`POST /api/v1/rpc` with
  `service.management_api.auth_token`) is the reliable way to launch a specific
  entry in a headless session. Also note the grid **re-lays out** when Steam
  un-gates itself at ~120s, which moves every tile after it.

## State left on the VM

Reversible, and left in place so a re-run need not redo it:

- `ppa:libretro/testing`, plus `retroarch`, `retroarch-assets`,
  `libretro-core-info`, `libretro-mgba`, `libretro-mupen64plus-next`,
  `okular`, `okular-extra-backends` (+ Qt5/Qt6/Breeze dependencies). Remove the
  PPA with `sudo add-apt-repository --remove ppa:libretro/testing`.
- `shepherd-dev` added to the `shepherd-firewall` group.
- `~/Games/retroarch/gba-tests-arm.gba`, `~/Books/alice.epub`,
  `~/Videos/shepherd-arm64-testclip.mp4`, and a `~/.config/shepherd/movies.toml`
  pointing at the last of these (it previously held the unedited example with
  `CHANGE-ME` paths).
- `dpkg --add-architecture amd64` and the amd64 cross set, alongside the
  restored native one. `/etc/apt/sources.list.d/` holds the PPA and is otherwise
  unmodified; the riscv64 exercise above was fully reverted (sources restored to
  their pre-run md5sums, ports entry deleted, `dpkg --remove-architecture`).

# Firewall rules not enforcing for Flatpak/Snap — misaligned embedded BPF object (#151)

Prompt: "investigate #151".

## Report

<https://git.armeafamily.com/albert/shepherd-launcher/issues/151>: an OpenRCT2
Flatpak configured with `[entries.firewall] default = "deny"` + loopback-only
allow rules still reached the online server browser. The journal shows the
privileged helper being invoked correctly and then failing:

```
pkexec[69227]: shepherd-user: Executing command [USER=root] ... [COMMAND=/usr/libexec/shepherd-firewall-helper apply-cgroup --cgroup-path .../app-flatpak-io.openrct2.OpenRCT2-138945749.scope --default deny --allow 127.0.0.0/8 --allow ::1/128]
WARN shepherd_host_linux::process: helper apply-cgroup failed scope=app-flatpak-io.openrct2.OpenRCT2-138945749.scope stderr=shepherd-firewall-helper: apply-cgroup failed: aya load: error parsing BPF object: error parsing ELF data: ParseError(ElfError(Error("Invalid ELF header size or alignment")))
```

Install: from-source build of bb3121e.

## Root cause

`crates/shepherd-firewall-helper/src/bpf.rs:31`

```rust
const BPF_OBJ: &[u8] = include_bytes!(env!("SHEPHERD_FIREWALL_BPF_OBJ"));
```

`include_bytes!` produces a `[u8; N]`, i.e. alignment 1. aya hands that slice
straight to the `object` crate, whose ELF header parse is a zero-copy cast:

- `object-0.36.5/src/read/elf/file.rs:530` — `data.read_at::<Self>(0)`,
  `.read_error("Invalid ELF header size or alignment")`
- `object-0.36.5/src/pod.rs:33` — `if (ptr as usize) % mem::align_of::<T>() != 0 { return Err(()) }`

`elf::FileHeader64<Endianness>` has alignment 8, so the load succeeds only if
the linker happens to place the embedded blob on an 8-byte boundary. It is a
build-layout coin flip: any unrelated change to the helper's rodata can flip it.

That also explains the standing comment above the `Ebpf::load` call claiming
that removing the `{e:?}` from the error closure "consistently" broke the load
via "monomorphization". It was never a monomorphization effect — adding/removing
the `Debug` formatting changed rodata layout, which moved the blob on or off an
8-byte boundary.

### Proof

Measured on the helper installed on shepherd-26.04 (an independently built
copy, so this is not specific to the reporter's box):

```
$ # locate the embedded EM_BPF (247) ELF inside the helper binary
off=0x1953c machine=247 sec=.rodata vaddr=0x1953c vaddr%8=4
```

The blob itself is well-formed — extracting bytes `[0x1953c, +3008)` and running
`readelf -h -S` on them gives a valid `ELF64 REL, Machine: Linux BPF` object with
`.text`, `cgroup/skb`, `.relcgroup/skb`, `license`, and `maps` sections. Nothing
is wrong with the BPF program or the build; only its address is wrong (4 mod 8).

## Blast radius

- Affected: `apply-cgroup` only — the Flatpak and Snap paths
  (`apply_firewall_to_existing_scope`, `crates/shepherd-host-linux/src/process.rs:318`).
- Not affected: the `process`-kind path (`apply-process`), which `exec`s
  `systemd-run --scope --property=IPAddress*=…` and never touches the embedded
  object.
- Steam entries are already documented as unsupported for firewall.

So the reporter's guess of "global firewall issue" is half right: it is not
Flatpak-specific, but it is confined to the aya/cgroup path.

## Secondary findings

1. **The cgroup path fails open, silently.** In
   `crates/shepherd-host-linux/src/adapter.rs:1626`, the firewall is applied in
   a detached `tokio::spawn` *after* the app has already launched; a helper
   failure only logs `warn!` (`process.rs:360`). The app keeps running with full
   network access and nothing surfaces to the parent. The process-kind path
   fails closed by comparison (enforcement status gates the launch, #143).
2. **No CI coverage for this path.** `.github/workflows/ci.yml:337` runs
   `firewall_real` (the systemd-run path) in a privileged sidecar. The tests that
   would have caught this — `firewall_real_snap.rs` / `firewall_real_flatpak.rs`
   — are `#[ignore]`d and require a hand-provisioned snap/flatpak.

## Fix (landed)

aya ships the standard idiom for exactly this
(`aya-0.13.1/src/util.rs:344`, a `#[repr(align(32))]` wrapper):

```rust
static BPF_OBJ: &[u8] = include_bytes_aligned!(env!("SHEPHERD_FIREWALL_BPF_OBJ"));
```

The `{e:?}`-monomorphization comment was replaced with a note recording the
real cause. The `{e:?}` itself stays: aya's `ParseError::ElfError` doesn't
expose its inner error via `source()`, so `Debug` is the only way to see why a
load failed.

`bpf::tests::embedded_bpf_object_is_aligned_and_parses` guards it: asserts
`BPF_OBJ.as_ptr() % 8 == 0`, then calls `Ebpf::load` and fails only on
`EbpfError::ParseError`, since everything past the parse (map creation, the
verifier) needs `CAP_BPF` that an unprivileged test run doesn't have.

Verified:

- With the fix, the test passes; reverting just the include back to
  `include_bytes!` fails it (`left: 6, right: 0` — the blob landed at 6 mod 8
  in that build of the test binary), so the test really does catch the bug.
- The release artifact is fixed, not just the test binary: the embedded EM_BPF
  blob in `target/release/shepherd-firewall-helper` now sits at vaddr
  `0x19b80`, 0 mod 32.
- `cargo fmt --all --check`, `cargo clippy -p shepherd-firewall-helper
  --all-targets -- -D warnings`, and `cargo test --workspace` all pass.

## Fail closed (landed)

Second prompt: "also make it fail closed".

`LinuxHost::spawn_firewall_guard` (`crates/shepherd-host-linux/src/adapter.rs`)
replaces the fire-and-forget `tokio::spawn` that used to apply the firewall to a
runtime-managed scope. On failure — the scope never appears, or the helper's
`apply-cgroup` fails — it now kills the activity and ends the session as
`HostEvent::LaunchFailed`.

Why that shape:

- **`LaunchFailed`, not `Exited`.** The engine skips usage settlement for a
  launch failure (`engine.rs`, issue #135), so a child is not billed for an
  activity that was yanked out from under them. The error string reaches the
  audit log and says what actually failed.
- **Killed via `kill_activity`**, which knows the snap/flatpak cgroup routes —
  signalling our direct child would miss an app the runtime put in a scope of
  its own.
- **Survivors go to the reconciliation sweep.** If the activity outlives the
  kill, `register_escaped_in` hands it to the same rescue arc a stuck stop uses
  (#136), rather than leaving an unfiltered activity running with nothing
  supervising it. `register_escaped` was split into a `&self` wrapper plus an
  associated fn so a background task can reach it.
- **An already-exited activity is left alone.** A child who closed the app
  during the attach window did not suffer a launch failure, and ending the
  session twice would end whatever launched next.

`apply_firewall_to_existing_scope` now returns `Result<String, String>` instead
of `Option<String>` so the caller can say *why* in the end reason.

The trade this makes explicit: on a host where the runtime never creates a scope
(no systemd user manager, say), firewalled snap/flatpak activities now die ~5s
in, loudly, instead of running unprotected, quietly. That is the point.

Two tests in `adapter.rs`, both driving a real process:

- `an_unappliable_firewall_ends_the_activity` — a scope prefix nothing will ever
  create; asserts the activity is dead and exactly one `LaunchFailed` carrying
  the reason. Mutating the guard back to fail-open fails it.
- `an_already_exited_activity_is_not_reported_as_a_launch_failure`.

Both collect results, clean up the survivor, and *then* assert: a failing
assertion that leaves `tail -f` holding the harness's stdout pipe hangs the run
instead of reporting it. (Learned the hard way while mutation-testing this.)

## Making the runtime-scope path testable in CI

Still the gap: nothing in CI exercises `apply-cgroup` at all. The `firewall` job
(`.github/workflows/ci.yml`) runs `firewall_real`, which is the `systemd-run`
process path — a completely different mechanism that never loads the embedded
BPF object. #151 was invisible to it.

The job already boots a `--privileged --cgroupns=host` sidecar with systemd as
PID 1, polkit, and the helper installed via `shepherd install firewall`, so the
hard part is done. Options, cheapest first:

1. **`wait_for_scope` against a fake hierarchy** (unit, unprivileged). Point it
   at a temp dir and assert the `app-flatpak-<id>-` / `snap.<n>.<n>-` patterns
   match what the runtimes actually name their scopes. Covers the naming
   contract only, but costs nothing.
2. **Drive `apply-cgroup` directly against a synthetic cgroup** — *implemented*,
   see below.
3. **The real flatpak probe** — which is what the third prompt asked about, and
   the answer is that it already exists: `scripts/integration-tests/
   test-firewall-flatpak.sh` builds `org.shepherd.firewall.Probe`, a flatpak
   whose entire payload is `run-firewall-probe.sh` (bash `/dev/tcp` against one
   allowed and one denied target), installs it user-scoped, and drives
   `firewall_real_flatpak.rs`. Nothing needs writing; it needs CI plumbing:
   - flatpak + a runtime in the sidecar image. Bake them into the base image
     built by the `images` job rather than downloading ~1 GB per run.
   - `flatpak-builder` and `org.freedesktop.Sdk` are avoidable for an app whose
     only content is a shell script: `flatpak build-init <dir> <app-id>
     org.freedesktop.Platform org.freedesktop.Platform 24.08` uses the runtime
     as its own SDK and drops the larger download.
   - A systemd **user** manager for the test user. The scope this path polls for
     lives under `user@<uid>.service/app.slice`; `runuser` alone does not start
     one. `loginctl enable-linger tester` plus `XDG_RUNTIME_DIR` /
     `DBUS_SESSION_BUS_ADDRESS`. (The process path in `firewall_real` sidesteps
     this: its scope is created on the *system* manager via pkexec.)
   - `/dev/fuse` for flatpak's revokefs — covered by `--privileged`.
   - Make the deny target hermetic. Both scripts default to `8.8.8.8:53` and
     skip when it is unreachable, which in CI reads as a pass. Bind a listener
     on the container's own non-loopback address and point
     `SHEPHERD_FIREWALL_PROBE_DENY` at it: the allow list is loopback-only, so
     it must be blocked, and the pre-flight from outside the sandbox proves it
     was reachable to begin with. No internet required, and no silent skip.
4. **Snap** is the same story via `test-firewall-snap.sh`, but snapd in a
   container is far more trouble than flatpak. Not worth it if (2) and (3) land.

Worth pairing any of these with a check that the *helper* is what CI thinks it
is: #151 shipped a helper whose embedded object could not be parsed, and every
existing test either skipped or warned.

## CI coverage for the cgroup path (landed)

Third prompt: "do option 2".

`crates/shepherd-e2e/tests/firewall_cgroup.rs` +
`scripts/integration-tests/test-firewall-cgroup.sh`, wired into the existing
`firewall` job in `.github/workflows/ci.yml` (as root, before the job hands the
workspace to `tester` for `firewall_real`).

What it does:

1. Creates `/sys/fs/cgroup/user.slice/user-61000.slice/user@61000.service/
   app.slice/<name>.scope` by hand. The uid is synthetic on purpose — the
   helper only uses `PKEXEC_UID` to bound the subtree it will touch, and
   borrowing a real user's tree would mean creating and removing cgroups under
   a live login session. Every directory it creates, it removes.
2. Runs the helper's `apply-cgroup` against it with `--default deny --allow
   127.0.0.0/8 --allow ::1/128`, `PKEXEC_UID` set. **This is the step that
   fails on a #151 build.**
3. Runs the existing `run-firewall-probe.sh` inside the cgroup
   (`echo $$ > cgroup.procs && exec bash probe.sh`, `HOLD_SECONDS=0`) and
   asserts `allow=OPEN`, `deny=BLOCKED`.

Hermetic by construction: the allowed target is a loopback listener, the denied
target is a listener on *this host's own* routable IPv4 (found by connecting a
UDP socket to `192.0.2.1:9`, which sends nothing but consults the routing
table). Nothing leaves the machine, no internet is needed, and a pre-flight
connect from outside the cgroup proves the denied target was reachable to begin
with — otherwise `BLOCKED` would prove nothing.

`SHEPHERD_FIREWALL_CGROUP_REQUIRED=1` turns every "not applicable here" skip
into a failure. CI sets it. This is the fix for the failure mode the other
firewall tests have, where an unmet precondition prints `[SKIP]` and exits 0.

### The read-only cgroupfs in CI

The first CI run failed exactly where the "watch this" note said it might:

```
Error: create cgroup /sys/fs/cgroup/user.slice
Caused by: Read-only file system (os error 30)
```

`/sys/fs/cgroup` is mounted read-only in the docker-in-docker sidecar, and its
root has no `user.slice` to begin with. Assuming a writable systemd-shaped
hierarchy was wrong.

The fix is not to relax what the helper accepts — its path check
(`/sys/fs/cgroup/user.slice/user-<uid>.slice/user@<uid>.service/…`) is a
security boundary, and the test should exercise the real one. Instead:
**mounting cgroup2 a second time gives a writable view of the same hierarchy.**
A cgroup created through that view is the same cgroup, and shows up under the
read-only `/sys/fs/cgroup` path — which is all the helper needs, since it only
ever opens the cgroup read-only (`File::open` + `BPF_PROG_ATTACH`).

So `open_write_view()` asks whether `/sys/fs/cgroup` accepts a new cgroup (by
creating one, not by trusting mount flags), and if not, mounts a private
cgroup2 at a temp dir and writes through that, unmounting on drop. The helper
is handed the `/sys/fs/cgroup` path either way, and `make_cgroup` refuses to
continue if the leaf is not visible through both views.

Reproduced locally before and after, since CI is a slow way to test this:

```sh
sudo unshare -m --propagation private bash -c '
  mount --bind /sys/fs/cgroup /sys/fs/cgroup
  mount -o remount,bind,ro /sys/fs/cgroup
  ...run the test binary...'
```

Read-only there, exactly like CI. The test now passes on both paths and says
which one it took.

Verified three ways on `shepherd-26.04` (kernel 7.0, cgroup v2):

- against the fixed helper: passes (6.2s, dominated by the probe's 6s deny
  timeout);
- against a helper reverted to plain `include_bytes!` **and padded so the blob
  lands at 1 mod 8**: fails with the issue's exact text, `ParseError(ElfError(
  Error("Invalid ELF header size or alignment")))`. Worth noting that the
  *unpadded* revert happened to land aligned in that build and passed — which
  is the whole character of this bug, and why the alignment assertion in the
  helper's own unit test matters as much as this test does;
- against a no-op helper that exits 0 without attaching anything: fails with
  `denied target ... was OPEN, expected BLOCKED`, so the assertion is not
  vacuous.

Left undone from the options list: the `wait_for_scope` unit test (1), the
flatpak probe in CI (3), and snap (4).

## Forcing the read-only path to run every time

The CI run that went green (PR #152, `f2a7814`) printed `[info] creating
cgroups directly under /sys/fs/cgroup` — the *writable* path. So whether the
sidecar hands us a writable cgroupfs varies between runs of the same job: one
failed with EROFS, the next did not. That left the private-mount fallback as
the least-exercised code in the firewall path, due to run for the first time on
whichever future run happened to land on a read-only host.

The `firewall` job now runs `firewall_cgroup` twice: once as the host gives it,
once inside `unshare -m --propagation private` with a read-only bind remount of
`/sys/fs/cgroup` (the same reproduction used while developing it). Two guards
keep the second run honest, because a silently-ineffective remount would just
re-run the first case and look like coverage:

- a `mkdir` that must fail, proving the fs really is read-only;
- a `grep` for `private cgroup2 mount` in the output, proving the run really
  took the fallback.

Both were checked against their negative case: dropping the remount makes the
first guard fail with
`::error::could-not-make-cgroupfs-read-only-fallback-not-exercised`. Costs about
6s.

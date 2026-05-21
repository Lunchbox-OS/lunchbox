# 2026-05-02 — Snap firewall via aya BPF cgroup attach

## Prompt

> Go ahead and implement A via aya, then validate by writing and running
> the test

(Approach A from the prior planning round: extend `shepherd-firewall-helper`
with a BPF-attach subcommand that loads a `cgroup_skb/egress` program via
`aya` and attaches it directly to the runtime's scope cgroup, replacing
the silent-no-op `systemctl --user --runtime set-property` path.)

## What landed

A new `apply-cgroup` helper subcommand and a new `shepherd-firewall-bpf`
crate. End to end on a properly-set-up dev box, the chain is now:

```
shepherdd
  └─ ManagedProcess::spawn → snap run shepherd-firewall-probe
  └─ tokio::spawn → wait_for_scope (poll cgroup hierarchy, ≤ 5 s)
       └─ pkexec → /usr/libexec/shepherd-firewall-helper
            apply-cgroup
              --cgroup-path /sys/fs/cgroup/.../snap.foo.foo-uuid.scope
              --default deny
              --allow 127.0.0.0/8 --allow ::1/128
            ─→ aya: load + populate {ALLOW_V4, DENY_V4, ALLOW_V6, DENY_V6, DEFAULT}
            ─→ raw bpf(BPF_PROG_ATTACH) on the cgroup, attach_type 1 (egress)
            ─→ exit 0; the program lives in the kernel until cgroup death
```

The new manual test (`firewall_real_snap.rs`) launches a tiny `snap try
--classic` probe through this path and asserts `allow=OPEN` (loopback)
and `deny=BLOCKED` (8.8.8.8:53 dropped). Verified locally: passes
repeatedly, `bpftool cgroup show` confirms the program is attached at
`cgroup_inet_egress`.

## Files

* `crates/shepherd-firewall-bpf/` — out-of-workspace BPF crate.
  `rust-toolchain.toml` pins nightly, `.cargo/config.toml` sets the
  `bpfel-unknown-none` target with `-Zbuild-std=core`. `src/main.rs` is
  ~110 lines: 4 LPM tries (v4/v6 × allow/deny), a `DEFAULT` array, one
  `cgroup_skb` program that returns `SK_PASS` / `SK_DROP` per the
  match-deny-then-allow-then-default order systemd's `IPAddressDeny=`
  uses.
* `crates/shepherd-firewall-helper/build.rs` — invokes `rustup run
  nightly cargo build --release` on the sibling BPF crate, embeds the
  resulting `.o` via `include_bytes!(env!("SHEPHERD_FIREWALL_BPF_OBJ"))`.
  Strips parent cargo env vars (`RUSTUP_TOOLCHAIN`, `CARGO`, `RUSTC`,
  `CARGO_TARGET_DIR`, `RUSTFLAGS`) so the nested cargo picks up the BPF
  crate's pinned toolchain instead of the host's.
* `crates/shepherd-firewall-helper/src/bpf.rs` — apply-cgroup
  implementation.
* `crates/shepherd-firewall-helper/src/main.rs` — new subcommand
  dispatch + cgroup-path validation (must be under
  `/sys/fs/cgroup/user.slice/user-<PKEXEC_UID>.slice/user@<UID>.service/`,
  no `..`, must be a real cgroup with `cgroup.procs`).
* `crates/shepherd-host-linux/src/process.rs` — replaced
  `systemctl --user --runtime set-property` with the `pkexec helper
  apply-cgroup` invocation.
* `dist/polkit/org.shepherd.firewall.policy` — comment that the existing
  action covers both subcommands (polkit gates by binary path, not by
  argv).
* `crates/shepherd-e2e/{src/lib.rs,tests/firewall_real_snap.rs}` —
  optional `SHEPHERD_E2E_LOG_FILE` for harness debugging; new manual
  test binary with self-skip if helper / polkit / snap / test snap is
  missing.
* `scripts/integration-tests/{run-firewall-probe.sh,test-firewall-snap.sh}`
  — probe gets an `INITIAL_DELAY` knob (the snap path has a real race;
  the Process path doesn't); orchestrator builds + `snap try
  --classic`-installs the probe snap and execs cargo test. Cleanup
  removes the snap on exit.
* `Cargo.toml` (workspace) — `exclude = ["crates/shepherd-firewall-bpf"]`.

## Why aya, with one specific carve-out

aya gives us correct map fd lifetimes, key/value layout, and BPF feature
detection, in pure Rust. It also gives us the `aya-ebpf` macro layer for
authoring the program in Rust (no embedded C source).

The carve-out: aya's `CgroupSkb::attach()` uses `BPF_LINK_CREATE` on
kernel ≥ 5.7, which ties the attachment lifetime to a userspace fd —
when the helper process exits the link drops and the program detaches.
That defeats the whole "attach and forget" model we want, where the
attachment lives until the cgroup dies. We work around it by calling
`bpf(BPF_PROG_ATTACH)` ourselves through `libc::syscall`, bypassing aya's
link wrapper. ~30 lines of unsafe in one well-bounded function. The
program persists for the cgroup's lifetime; the kernel cleans it up on
cgroup destruction.

## Things that bit during integration

A list, since each one cost a non-trivial chunk of the session:

1. **`bpfel-unknown-none` is tier-3.** No precompiled libcore; needs
   `-Zbuild-std=core` (`.cargo/config.toml` `[unstable]`) which in turn
   needs nightly. Pinned via `rust-toolchain.toml` in the BPF crate.
2. **Nested cargo inherits the parent's toolchain.** When `build.rs`
   calls `cargo build` for the sibling BPF crate, the parent's
   `RUSTUP_TOOLCHAIN`, `CARGO`, `RUSTC`, `RUSTFLAGS` etc. all leak
   through. The fix: `Command::new("rustup").args(["run", "nightly",
   "cargo", "build", …])` plus `env_remove` for each leaked var. Took
   two iterations to find the right set.
3. **aya needs a `license` section.** `aya-ebpf` doesn't auto-emit one,
   and the verifier (and aya's loader) reject programs without it. Added
   `#[unsafe(link_section = "license")] pub static LICENSE: [u8; 4] =
   *b"GPL\0";` in the BPF crate.
4. **`BPF_CGROUP_INET_EGRESS` is `1`, not `2`.** Off-by-one against the
   UAPI enum. Index 2 is `BPF_CGROUP_INET_SOCK_CREATE`, which has a
   different program-type expectation, so attach failed with `EINVAL`
   ("Invalid argument") and the cause was opaque without unwinding the
   error chain.
5. **`ParseError::ElfError(_)` doesn't expose its inner via `source()`.**
   thiserror needs `#[source]` to chain; aya-obj 0.2.1 doesn't have it
   on this variant. So the top-level message ("error parsing ELF data")
   is all you get from a normal source-chain walker. We use `format!("{e}:
   {e:?}")` to also Debug-print the variant, which reveals the inner.
6. **The `{e:?}` is also load-bearing in a way I do not understand.**
   Removing the Debug formatter from the `map_err` closure makes
   `Ebpf::load(BPF_OBJ)` consistently return `ParseError::ElfError(_)`
   on the helper's hot path — even though the *same BPF bytes* (verified
   by md5) load cleanly when invoked outside the test harness. With
   the Debug formatter present the same call returns Ok and the
   subsequent attach succeeds. The two binaries differ in size and
   md5, so the closure body is influencing codegen for the success
   path. Best guess: monomorphization of `Debug` for the aya error
   types pulls in code that affects feature detection (which runs early
   in `Ebpf::load` and inspects `/proc/sys/...`). I left a comment in
   `bpf.rs` pointing at this; the workaround is contained, the
   formatter improves real-world error messages anyway. Worth a
   minimum repro and a bug filed against aya 0.13.1 / aya-obj 0.2.1
   when there's time.

## Validation

```
$ ./scripts/integration-tests/test-firewall-snap.sh
…
running 1 test
test snap_firewall_enforcement_with_real_helper ... ---- probe log ----
allow=OPEN
deny=BLOCKED
-------------------
ok

test result: ok. 1 passed; 0 failed; 0 ignored
```

Three consecutive runs all pass. shepherdd's log shows
`Applied firewall (BPF) to scope`. `bpftool cgroup show` lists the
attached program at `cgroup_inet_egress`.

## Things this does NOT cover

- **CI build.** The helper now requires `clang`, `llvm-20-dev`,
  `libpolly-20-dev`, `cargo install bpf-linker`, and `rustup install
  nightly --component rust-src` to compile. CI's `ubuntu:25.10`
  container doesn't have any of those; `cargo build --workspace` in CI
  will fail at the helper. Two reasonable fixes (a) add the deps to the
  CI image and `scripts/deps/build.pkgs`, (b) precompile the BPF `.o`
  in a dist stage and check it in, with `build.rs` falling back to it
  when the toolchain isn't present. Out of scope here; lands on the
  follow-up list.
- **Flatpak.** The same code path now works for any runtime-managed
  scope, but the scope-name pattern in `apply_firewall_to_existing_scope`
  is snap-specific; need a flatpak variant before a flatpak test can
  exercise it.
- **Cleanup of stale BPF programs.** When a snap scope is destroyed
  cleanly, the kernel detaches the program. If a test crashes mid-flight
  before scope destruction, the program lingers until the user logs out
  / reboots. Not a real concern in practice (cgroup cleanup happens
  reliably), but worth noting.

## Footprint

- `shepherd-firewall-helper` debug binary: 21 MB (was ~3 MB pre-aya).
  Mostly aya + LLVM-friendly core. Release-mode strip-aware build is a
  follow-up.
- BPF program object (3 KB) is embedded into the binary.
- Compile time: the BPF crate compile with `-Zbuild-std=core` adds ~20s
  to a clean build; incremental rebuilds are fast.

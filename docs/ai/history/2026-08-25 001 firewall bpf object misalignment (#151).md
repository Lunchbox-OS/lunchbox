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

## Still open

Not part of this fix, and worth deciding separately:

- **Fail closed.** A failed `apply-cgroup` still only logs `warn!`
  (`crates/shepherd-host-linux/src/process.rs:360`), and the activity keeps
  running with unrestricted network — which is why this bug was invisible until
  a child opened a server browser. The process-kind path fails closed. The
  cgroup path should probably terminate the activity and raise a diagnostic.
- **CI coverage.** The aya path has none. `firewall_real_snap.rs` /
  `firewall_real_flatpak.rs` are `#[ignore]`d and need a hand-provisioned
  app; the new unit test covers only the parse, not attach-and-enforce.

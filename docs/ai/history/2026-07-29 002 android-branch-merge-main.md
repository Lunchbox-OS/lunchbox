# Issue 2: merging `origin/main` into the paused Android-activity branch

<https://git.armeafamily.com/albert/shepherd-launcher/issues/2>

## Prompt

> This is the Android activity support branch, but I paused development on it
> for a while. Merge in origin/main and address the conflicts

## Background

`u/albert/2/android-activity` had been idle while `main` moved on by 62 commits
(0.3.0 → 0.3.2). Merge base `cf5f90a`; merge commit `1432813`, parents
`baff668` (branch) + `e88670d` (main).

The two sides were mostly disjoint — the branch is additive Android/Waydroid
work, `main` landed grouped time limits + tokens (#5, #8), input-device
dependencies, the F-Droid repo, the apt registry, the `shepherd-wire-codegen`
crate, and media hardware decoding. Git reported 13 conflicted files, but only
a handful were substantive.

## Conflict resolution

**Version literals** (`VERSION`, `Cargo.toml`, `crates/shepherd-firewall-bpf/Cargo.toml`,
`shepherd-webui/package.json`, all three lockfiles) — took `main`'s 0.3.2.
Note `crates/shepherd-firewall-bpf/Cargo.lock` is *not* covered by
`shepherd version check` (only its `Cargo.toml` is), so on `main` it had drifted
to a stale 0.2.4; it was set to 0.3.2 to match its manifest.

**`Cargo.lock` package ordering** — both sides inserted a new package at the same
alphabetical position (`shepherd-waydroid-helper` vs `shepherd-wire-codegen`).
Kept both.

**Import lists** (`shepherd-config/src/policy.rs`, `shepherd-management/src/service.rs`)
— unioned; the branch added `RawWaydroidConfig` / `EntryKind`, `main` added
`RawInputDevice` / `GroupView` / `TokenStatus`.

**`shepherd-config/src/validation.rs`** — both sides appended tests to the same
`mod tests` tail. Kept both test sets.

**`.github/workflows/release.yml`** — both hunks additive: kept the branch's DPC
apk release asset *and* `main`'s apt-registry publish step, and merged the
secrets comment block.

**`docs/INSTALL.md`** — three conflicts, all additive. `main`'s hardware-decoding
and "Installing the Android apps" sections were kept alongside the branch's
Waydroid section; the shared "install an activity backend" sentence kept the
branch's Android-aware wording. `main`'s new "Input-device dependencies" section
was placed before the Waydroid one so it continues the `groups`/`udev`
discussion above it.

## Semantic conflicts (clean textual merge, broken build)

Two breakages that no conflict marker flagged:

1. `main` added `crates/shepherd-ble/src/testsupport.rs` with a `MockSvc`
   implementing `ManagementService`; the branch had added a `back()` method to
   that trait. Added the missing `back()` to the mock.
2. The branch's `android_entry_rejects_bad_package` test builds `RawEntry` /
   `RawConfig` struct literals; `main` added `group`, `tokens`,
   `requires_input`, and `groups` fields. Filled them in.

## Codegen

`cargo test -p shepherd-wire-codegen` failed on `codegen_outputs_match_checked_in`:
`main` moved codegen into the new `shepherd-wire-codegen` crate and added a
Kotlin wire-types target, which had never seen the branch's `EntryKind::Android`
or `SessionInfo.kind_tag`. Regenerating with
`cargo run -p shepherd-wire-codegen --bin rpc-codegen` updated only
`companion-android/.../domain/WireTypes.generated.kt` — the JSON schema and the
TypeScript output were already correct.

**Whenever a branch that adds an RPC method or wire type is merged forward, the
generated outputs must be regenerated — the drift test is the only thing that
catches it, and it is not in the conflict set.**

## Verification

`cargo check --all-targets`, `cargo clippy --all-targets -- -D warnings`,
`cargo fmt --all -- --check`, `cargo test --all-targets` (42 test binaries, 0
failures), `./scripts/shepherd version check`, `./scripts/shepherd config
validate config.example.toml`, `shellcheck` over the script set,
`./scripts/ci/check-arch-neutral.sh`, `npx tsc --noEmit` + `npm run build` in
`shepherd-webui`, and `./gradlew :app:testDebugUnitTest` in `companion-android`.

Two environment gotchas hit along the way:

- `./scripts/shepherd config validate` only builds `validate-config` when the
  binary is **missing**, so after a merge it happily validates against a stale
  binary. Force it with `cargo build --bin validate-config` first — the example
  config's `type = "android"` entry otherwise reports as an unknown variant.
- The companion-android Gradle build fails with a bare
  `IllegalArgumentException: 25.0.3` under this machine's default JDK 25.
  Kotlin's bundled compiler can't parse that version string. Build with
  `JAVA_HOME=/usr/lib/jvm/java-21-openjdk-amd64`, matching CI's JDK 21.

## Known gap (pre-existing, not a merge regression)

`shepherd-webui/src/api/types.ts` is hand-written and its `EntryKindTag` union
still lacks `"android"` — it is missing on both `baff668` and `e88670d`, so the
branch never added it. The web UI compiles and builds because nothing switches
exhaustively on the tag, but an Android entry's kind will not type-check if the
union is ever narrowed. Worth closing before the Android branch merges to main.

# Rebasing the Android branch onto main, and bumping to 0.6.0 (#2)

<https://git.armeafamily.com/albert/shepherd-launcher/pulls/75>

## Prompt

> #75 is currently checked out. Rebase it atop the current main, replacing the
> bump to 0.3.0 with a bump to 0.6.0, and reverify

## What this was

PR #75 (`u/albert/2/android-activity`, "Add support for Android activities") had
been kept current by **merging** `origin/main` into it seven times. The ask was
to replace that with a linear rebase onto `origin/main` at `0f73312` — 387
commits of main since the fork point at `c3204f0`, against 78 branch commits.

Dropping the seven merges is what makes this expensive: every conflict those
merges had already resolved comes back, against a main that has moved on again
since. The result is 76 commits (two dropped, see below) plus four new ones.

## The mechanics

`git rebase main` with `rerere.enabled` + `rerere.autoupdate`. The branch's own
commits are exactly `main..HEAD` minus merges, so nothing from main is replayed.

Two commits were dropped as obsolete rather than resolved:

- `f8feaeb` "sync Cargo.lock waydroid-helper version to workspace 0.2.0" — the
  lock now carries the workspace version anyway.
- `3a5924c` "ci: test and lint the whole workspace" — main got there first, and
  documents it better in CONTRIBUTING than the branch's version did.

**`git commit --amend` during a rebase amends the last *applied* commit, not the
one that is stopped and unstaged.** Doing that at the version-bump commit
silently swallowed the preceding `fix(android): satisfy the arch-neutrality
check` into it; it was split back out with `reset --soft` + two commits. Worth
knowing before reaching for `--amend` at a rebase stop: use
`git restore --source=HEAD --staged --worktree .` to get back to the applied
tree, make the change, and commit normally.

The four `#[allow]`/formatting/integration follow-ups that belonged *inside*
existing commits went in as `git commit --fixup=<sha>` + `rebase --autosquash`,
so the shipped history has no "fix up the rebase" noise in it.

## What only a rebase surfaces

A merge resolves a conflict once; a rebase re-resolves it at each commit that
touched the region, which is why a few of these look like ordinary conflicts and
are really main and the branch disagreeing about a design.

**Main deleted the thing the branch was editing.**

- `scripts/lib/package.sh`: the branch added the Waydroid polkit rule to
  `conffiles`; main removed `conffiles` entirely (#177 — vendor files do not
  belong in admin directories). Resolution: main's, and the branch's line goes
  with it.
- `shepherd-webui/src/api/types.ts`: the branch patched a hand-written
  `EntryKindTag` union; main generates that file now. Resolution: drop the patch
  and let `rpc-codegen` produce the union.
- `CONTRIBUTING.md`: the branch's JDK-21 note is a subset of main's, which also
  covers `ANDROID_SDK_ROOT` and quotes the bare-version-string failure.

**Main renamed or reshaped what the branch called.** `SwaymsgBackend` →
`SwayIpcBackend`; `display_watch` moved from spawning `swaymsg -t subscribe` to
holding the IPC connection itself (#147), so the branch's Waydroid re-pin hook
had to be re-attached to main's event loop rather than to the `swaymsg` reader;
`notify_session_exited` → `notify_launch_failed`; the spawn-parameter resolution
in `shepherd-management` moved into a shared `resolve_spawn`, so the Android
`needs_hidpi` rule now applies to both launch paths instead of only the
management one.

**Semantic conflicts — merged clean, broke the build.** These are the ones the
conflict set cannot show you, and each was caught by running
`cargo check --workspace --all-targets` at the stop, not at the end:

- `EntryKindTag::ALL` and `as_str` (main, for the config editor) did not know
  about `Android`.
- Main's `schema` feature needs `JsonSchema` on every `Raw*`;
  `RawWaydroidConfig` had no derive.
- `SessionInfo`/`SpawnOptions`/`RawEntry`/`ServiceStateSnapshot` all grew fields
  on main that the branch's literals in tests did not set.
- `HidpiController::apply` returns `f64` now (the branch's change); a test mock
  in `shepherd-management` still returned `()`.

**Main added a lint the branch violates.** `clippy.toml` bans
`Command::new` (#144): naming a binary lets `$PATH` — which the kiosk user
controls — choose it. `crates/shepherd-host-linux/src/waydroid.rs` named
`waydroid` and `pkexec` from inside the daemon, which is the real attack, not a
lint nit: the whole point of the privileged helper is that it is reached through
polkit. Every call site now goes through `helpers::tokio_command`. The two
genuine exceptions (the pkexec'd helper itself, and the manual integration
fixture) take a scoped `#[allow]` with the reason, as the firewall helper's do.

**Main's config editor is a compile-time drift guard, and it worked.** Adding
`android` to the schema broke `KIND_LABELS` / `KIND_HINTS` / `blankKind` in
`shepherd-webui/src/config/model/kinds.ts`, exactly as their comment promises.
Filling those in is not sufficient: `KindEditor` renders per-kind fields behind
non-exhaustive `kind.type === …` guards, so an Android activity would have been
creatable with no `package_name` field to type into — the same shape as #192.

## The version bump

`56b639a` "chore: bump version to 0.3.0" became "bump version to 0.6.0", run
through `./scripts/shepherd version set 0.6.0` on the rebased tree rather than
by resolving its conflicts (main is at 0.5.1 and the file set has changed). The
illustrative `.deb`/`.apk` versions in `docs/INSTALL.md` were bumped with it, as
the original commit did.

## New commits on top

1. `chore(wire): regenerate the wire outputs after the rebase onto main` —
   generated files are never in a conflict set, so a rebase leaves them stale and
   only the drift test (which needs `--workspace`) notices.
2. `fix(android): resolve Waydroid's host commands on the trusted path` — the
   `$PATH` fix above.
3. `feat(config-editor): offer the Android kind in the web config editor`.

## Verification on the rebased tree

`cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D
warnings`; `cargo test --workspace --all-targets` (**71 test binaries, 0
failures**); `./scripts/shepherd version check`; `./scripts/shepherd config
validate config.example.toml`; `./scripts/ci/check-arch-neutral.sh`;
`./scripts/ci/check-workflows.sh`; CI's exact shellcheck invocation;
`npx tsc --noEmit` and `npm test` (116 cases) in `shepherd-webui`;
`:app:testDebugUnitTest` in `companion-android` (JDK 21 + `ANDROID_HOME`).

End-to-end in the headless session (`dev headless` → `dev shot` → `dev stop`),
which exercised the Android path for real because `config.example.toml` has an
Android entry and this box has Waydroid installed:

- `startup_busy` went `true` → `false`; the launcher covered the grid with its
  loading page for the whole pre-boot and uncovered afterwards.
- Pre-boot ran its full path, including the branch's newest fix — "Waydroid props
  could not be set before a session existed; restarting once to apply them" —
  and finished with "Waydroid pre-boot complete; session warm".
- The Android entry reports `kind_tag: Android` and stays gated on
  `NotReady { kind: Android }`, which is correct here: boot-completion is read
  through the privileged helper, which is not installed on this box.

## Environment note

`target/` had grown to 32 GB and filled the disk mid-run (`No space left on
device` from rustc, and a `ld` bus error that looks like an LLVM crash).
`rm -rf target/debug/incremental` freed 15 GB without discarding the dependency
artifacts; the rest of the run used `CARGO_INCREMENTAL=0`.

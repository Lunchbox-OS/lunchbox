# State custodian — the review that followed the rebase (issue #157, PR #161)

Continues [2026-09-05 004](./2026-09-05%20004%20state-custodian-rebase-reverify.md),
which covered only the rebase. What began as "rebase and reverify" turned into a
review of the branch by question-and-answer, and the branch grew from 12 commits
to 28. This note records what the review found and what changed because of it,
so the next person does not have to reconstruct it from the log.

The shape of the session is worth naming: almost nothing here came from
re-reading the diff. It came from answering questions about it — "what does
installing do now", "who manages the bond's files", "how much is `StateRequest`
duplicating", "what does `Degraded` provide now" — and finding that several
answers were uncomfortable.

## The rebases

Twice. First onto `e75344d`, recorded in the earlier note. Then onto `7796c8d`
once #176 merged mid-session, replaying all 23 commits with **no delta at all** —
the branch's own patch is byte-identical against either base, compared hunk by
hunk. #176 composes because the billed day is a *parameter*
(`add_usage(entry_id, day, duration)`) that `RemoteStore` forwards verbatim; a
store that derived the day itself would have been a real collision.

## What the review changed

### The policy had two homes, and a diagnostic to police them

The original design kept a real `config.toml` in the kiosk user's home as the
seed and the fallback, with `policy_diverged` reporting when it drifted from the
custodian's copy. The reporter's objection was that this is two files that look
equally authoritative, only one of which decides anything — and a diagnostic
whose whole job is to report the confusion the arrangement creates.

So migration now **moves** the policy and leaves a signpost naming where it
went, and `policy_diverged` is gone: `DiagnosticCode::PolicyDiverged`,
`ProbeFacts::policy_diverged`, `PolicySource`, and the generated TypeScript and
Kotlin with them. One file decides.

### The knobs that were the *other* other home

`admin_record_path` and `reset_sentinel_path` let an operator put the BLE admin
record and the reset sentinel anywhere. On a device with the custodian they were
already dead — `BleServer::new` took the other arm and never read them — while
staying editable in the config editor and documented in `config.example.toml`,
whose commented example pointed *outside* the custodian's directory.

Nothing in the tree set them. They are gone, and the two-variant `Backend` they
forced went with them: `AdminStore` and `PendingUnbondStore` take one
`Arc<dyn ProtectedFiles>`, and `Option<&Path>`, `pending_unbond_path` and the
branch in `BleServer::new` are deleted. 327 lines out, 169 in.

### The claim was per-user; the bond never was

Asked what happens on a device with two kiosk users. The custodian is templated
per user, so the admin record in its directory made the *claim* per-user — while
a BlueZ bond lives in `/var/lib/bluetooth` at one adapter and
`Adapter::remove_device` forgets it for the machine. A phone claimed for one user
arrived at the next already bonded and could claim it too; a factory reset for
one user took the system bond and unpaired the phone from the others.

`ProtectedFile::scope` now says whether a file describes a child or the machine.
The policy and database stay per-user; the admin record, unbond queue and reset
sentinel move to a shared `/var/lib/shepherdd/admin/`, created by the same
`StateDirectory=` at the same uid and mode. Migration merges (first user wins,
the rest reported); `--restore-to-home` un-merges by copying to every home.

`sudo touch /var/lib/shepherdd/admin/.factory-reset-ble` has no kiosk user in
its path any more, which is the point.

### A hardened device could not be reconfigured

`harden apply` gives the kiosk user `nologin` and denies it SSH, so there is no
`su` into it to edit a config — and `install policy` only ever read that user's
home. `--source` takes a policy from anywhere an administrator can name.

Both forms validate first, which needed `validate-config` to ship (renamed
`shepherd-validate-config`, since it was the only binary in the set without the
prefix and now sits on every device's `$PATH`).

### A packaged device could not do any of it

The `.deb` ships `shepherd-admin`, which has five subcommands and no `install`
verb — so `shepherd install state --user <user>` was advice a packaged operator
could not take, and `warn_if_stale_home_db` was giving exactly that advice.
`setup-user` enabled the socket and populated nothing.

`setup-user` now does the whole per-user half, and three verbs cover the rest:
`shepherd-admin policy`, `migrate-state`, `restore-state`. `restore-state`
deliberately does *less* than `uninstall state --restore-to-home` — it leaves the
binary and units for `apt`, because on a packaged device they are dpkg's.

### The fallback stopped being worth having

The last and largest reversal. The fallback was justified by "an unprotected
kiosk beats a child staring at a dead screen", which held while the home still
had a real policy and database. Once migration *moves* them, falling back opens
an empty database, finds no admin record, and reads a signpost that grants
nothing — so it produces a launcher with no activities.

The reporter's argument, which is the right one: that failure is **ambiguous**.
An empty grid reads as an ordinary evening with nothing available, or as
something still loading. Being returned to the greeter does not.

So a device whose custodian is installed and unreachable now exits. A device
that never had one still starts and reports `state_not_protected`, because
nothing has moved and refusing would be refusing over a protection it never had.

This deleted most of a commit written earlier the same session:
`ClaimError::StateUnreachable`, `claims_unreachable` through `ClaimMachine` and
`BleServerConfig`, and the `custodian_expected` field threaded through two enums
to reach them. All of it existed to make a degraded device survivable; a device
that does not come up cannot be claimed. 232 lines out, 128 in.

## One fact, one place

A theme rather than a change. Asked to audit for the shape the policy had — one
fact written twice with a comment holding the copies together — the branch had
four more, and one had already drifted into a bug.

| | |
| --- | --- |
| `unbond-queue.toml` in `ProtectedFile` but not in the installer's list | **a live bug**: a pending unbond survived an upgrade only where nothing reads it, so the bond it was queued to remove stayed — the one thing a factory reset is for |
| the systemd units restating `socket_path`, `state_dir`, `STATE_USER` | unguarded; a drift fails as a refused connection that reads exactly like "no custodian installed" |
| `bluetooth.sh` with its own copy of the custodian's directory | held together by a comment saying it "must match install.sh's" |
| the `Store` method list written out three times | the compiler checked two; the third — an arm calling the *wrong* method — typechecks |

The first two are now compared by tests
(`installer_covers_protected_files.rs`, `units_match_the_constants.rs`), the
third sources `install.sh`, and the fourth is generated from a table in
`shepherd-state-proto/src/methods.rs` — client and server for a method now come
from the same line and cannot disagree about which method that is.

The installer test was checked against the real bug by removing
`unbond-queue.toml` again and watching it fail with the file name and the array
to add it to. That is worth doing for any test written to catch a bug that
already happened.

## Two claims I made in this session that were wrong

Recorded because both were stated confidently and both are the kind of thing
that would otherwise be inherited.

**"A failed shepherdd loops on an autologin kiosk."** It does not. shepherdd
unlinks sway's IPC socket only *after* it has connected, so a startup failure —
a bad policy, say — still finds the socket and `swaymsg exit` still works. And
this project configures no autologin: the getty override `harden apply` writes
is inert, every line commented. The session ends at the greeter. The correction
came from the reporter's own experience of exactly that, and it is what made the
fail-closed argument above land the way it did.

**"A flaky test I could not reproduce."** It was `codegen_outputs_match_checked_in`,
twice, both times because a doc comment on a wire type had been edited — those
render into the generated Kotlin and TypeScript. `cargo test` stops at the first
failing target, so the run reported a short count (1105 of 1186) and looked like
an aborted run rather than a failed assertion. Regenerating fixed it both times.
The lesson is narrow and useful: after editing *any* doc comment in
`shepherd-api`, regenerate before trusting a test count.

## Verification

Every gate, on the final tree: `cargo test --workspace --all-targets` 1184
passed / 0 failed with `SHEPHERD_REQUIRE_PEER_CGROUP=1`, e2e 17, clippy, fmt,
shellcheck, web UI 83 with 137/137 schema fields reachable, companion
`:app:testDebugUnitTest`, version harmony, `config.example.toml` validates, and a
real `.deb` built and inspected (12 executables, all `shepherd*`).

The shell paths were exercised against temporary state roots rather than reasoned
about, since none of them run in the dev harness: migration and restore round
trips including a two-user fixture, `install policy` accepting a valid policy and
refusing an invalid one and a signpost, and the packaged per-user step.

The headless session was booted on the rebased branch — launcher maps, `health`
reports `policy_loaded`/`store_ok`/`ready`, a launch is approved and writes usage
keyed by day. The custodian itself remains outside that harness by design
(`--no-state-custodian`), so the boundary is still only proven on a device.

## Still open

- **Promote, never demote.** The fallback decision is startup-only, so a device
  that came up unprotected stays that way until the session restarts even after
  the custodian recovers. Monotonic promotion is safe but needs a decision about
  usage accrued in the local database.
- **`authorize` gates on the claim and lets any bonded peer through** — #149's
  answer to give, and it wants the resolved identity or the IRK.
- **Two simultaneous graphical sessions.** The custodian refuses two sessions for
  one uid, so two uids with one each satisfies it, and two `shepherdd`s would
  contend for one adapter's GATT registration. Untested.
- **Packaging `conffiles`** — a separate issue drafted this session, unrelated to
  #157 except that reviewing its packaging is where it surfaced. One of the four
  entries is rewritten by postinst, which makes every later upgrade see a
  locally-modified conffile.

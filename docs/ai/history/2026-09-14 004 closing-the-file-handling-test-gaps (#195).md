# Closing the file-handling test gaps (issue #195)

> Status: **done**, 2026-09-14. The audit that prompted it, the ten gaps it
> found, and what each one turned up when it was actually closed.

## Prompts

> how tested is the actual file handling
>
> what would it take to actually implement #10 there
>
> fixes first, then the harness, then all of the rest of the tests you suggested

## The audit

The feature had 49 route tests, 35 resolver and roots unit tests, and 67 UI
tests, all green. The honest summary was that **the protocol was well tested
and the storage under it was not**. Every test ran against a
`tempfile::tempdir()` — ext4 or tmpfs — holding ordinary files, one writer, and
nothing bigger than a few kilobytes. Ten gaps came out of that:

| # | Gap | Outcome |
| --- | --- | --- |
| 1 | `src/api/files.ts` had no test at all | 17 tests |
| 2 | Nothing larger than a few KiB ever moved | 24 MiB, whole and chunked |
| 3 | No e2e chapter: auth, `enabled`, the real denied dirs | 5 tests, **1 bug** |
| 4 | Filenames that are not UTF-8 | 1 test |
| 5 | Special files (fifo, socket, device) | 2 tests |
| 6 | Recursive delete with a symlink inside | 1 test |
| 7 | The 24-hour `.part` sweep | 1 test |
| 8 | Two writers at once | 3 tests, **1 bug** |
| 9 | The `dev`/`ino` check after `open` | 1 property test |
| 10 | A removable drive, for real | 8 tests, **5 bugs** |

Seven bugs, in a feature whose tests were all passing. Every one of them lived
in the gap between "the protocol is right" and "the storage under it behaves
the way the protocol assumes", and none was reachable from a tempdir.

## Order

Item 10 first, because a real FAT drive was the largest single blind spot and
the fixes it forced changed what the other tests should assert. That work —
the three bugs, the loopback harness, and the two more the harness found on its
first run — is written up separately in
`2026-09-14 003 what-a-real-fat-drive-found (#195).md`.

## The two bugs the rest of it found

### `If-None-Match: *` was checked, then not kept

`prepare_write` asks whether the name is free, and then 64 KiB of body arrives,
and then a plain `rename(2)` publishes the file. Between the question and the
answer another writer — a second administrator, or this caller's own retry
after a timeout — can create the same name. Both then answer `201` and one
person's file is silently gone.

The test that found it is deliberately shaped as an invariant rather than an
expectation: *exactly one creates, and what is on disk matches one writer
entirely*. That is true in every interleaving, which is what makes a
concurrency test deterministic.

The fix moves the promise into the kernel: `renameat2` with
`RENAME_NOREPLACE`. Filesystems without it — vfat among them, which matters
because removable drives are a first-class root — fall back to the plain
rename, so the window stays open on a USB stick and is closed on the device's
own disk. That is the right way round: the device's own disk is where two
administrators are both writing.

### The refused data directory was a guess

`denied_dirs` works the database's location out from `$XDG_DATA_HOME`. That is
correct on a device and wrong the moment anyone starts `shepherdd -d
/srv/library` or sets `service.data_dir`: the refusal then guards a path
nothing is at, and the real database — if it is inside the home — is
downloadable, and writable.

Only an end-to-end test can ask this, because the question is "is the directory
*this running process* opened refused", and an in-process test constructs the
answer it is checking. `FileService::also_deny` lets the daemon say where its
store really is instead of being second-guessed.

## Notes worth keeping

- **A fifo is the reason the download route stats before it opens.** `open(2)`
  on a fifo with no writer blocks forever, so the wrong order would hang a
  worker thread until the daemon was restarted. The test does not merely assert
  a `400`; it wraps the call in a timeout, so it asserts that an answer arrives
  at all.
- **A recursive delete must unlink a symlink, not walk it.** `remove_dir_all`
  does the right thing, but any activity at the kiosk uid can plant a link into
  a person's documents inside a folder they think they are tidying, so the
  behaviour is now pinned rather than inherited.
- **The `dev`/`ino` check cannot be raced on purpose**, so it is tested as a
  property instead: a writer replaces the file the way this API does, over and
  over, while forty downloads run — and every body is entirely one version or
  entirely the other. A `500` from the check is allowed; a mixture is not.
- **The transport tests are the other end of the same wire.** A precondition
  header spelt wrongly throws nothing — it clobbers a file. A `Content-Range`
  off by one throws nothing either — it desynchronises an upload that then
  re-sends four gigabytes. Both ends now have to agree in writing.

## Where each layer lives

| | What it is for |
| --- | --- |
| `shepherd-http/src/files/*` unit tests | path resolution, escapes, validators, errno mapping, `/proc/mounts` parsing |
| `shepherd-http/tests/files.rs` | the wire contract: statuses, preconditions, headers, escapes |
| `shepherd-http/tests/files_on_disk.rs` | the filesystem underneath it: odd files, concurrency, size |
| `shepherd-http/tests/files_removable.rs` | a real FAT drive (`#[ignore]`, `[SKIP]` without one) |
| `shepherd-e2e/tests/files.rs` | a real daemon: auth, `enabled`, the real denied dirs, a real socket |
| `shepherd-webui/src/api/files.test.ts` | the client's half of the same contract |
| `shepherd-webui/src/files/*.test.*` | what the tree and the uploads do with the answers |

## Still not covered, deliberately

- **`EFBIG` and `ENAMETOOLONG` against a real drive.** Neither is reachable
  through the API on a 512 MiB image; both are covered by the unit test on
  `from_io`. Written down because a test that appears to cover something and
  does not is worse than no test.
- **A file over 4 GiB.** The arithmetic is `u64` throughout and the 24 MiB
  tests exercise the same paths; an hour of CI per run buys nothing more.
- **Two daemons on one directory.** Out of scope: there is one.

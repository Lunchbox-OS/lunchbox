# What a real FAT drive found, and the fixes (issue #195)

> Status: **three bugs found on a loopback FAT image, all fixed**, 2026-09-14.
> The first chapter of the answer to "how tested is the actual file handling".

## Prompt

> how tested is the actual file handling
>
> what would it take to actually implement #10 there
>
> fixes first, then the harness, then all of the rest of the tests you
> suggested

Item 10 of that audit was "exercise a removable drive for real". Every test to
this point had run on a `tempfile::tempdir()` — that is, on ext4 or tmpfs,
which is the one filesystem this feature is *least* likely to be pointed at.
Removable drives are a first-class root here: `external_media = true` is the
default, and the roots list scans `/media` and `/run/media` for them.

A 512 MiB loopback image, `mkfs.vfat`-ed and mounted, found three bugs in the
first twenty minutes. None of them were subtle once seen, and none of them
could have been found on tmpfs.

## 1. The etag lies on FAT, and `If-Match` believed it

The etag is `size-mtime`. **FAT and exFAT store modification times at
two-second resolution**, so two different files of the same size written in
the same two-second tick carry *the same tag*.

That is not a theoretical hazard, it is the resume path:

```
GET  film.bin                 → 200, ETag: "40000000-1789…000"
     (connection cut at 8 MB)
     film.bin is replaced, same size, within the same tick
GET  film.bin
     Range: bytes=8000000-
     If-Match: "40000000-1789…000"   ← still matches!
                               → 206, bytes from the *new* file at the old offset
```

The browser then writes a file that is 8 MB of one version and 32 MB of
another and reports a successful download. This is exactly the corruption the
`If-Match` work in `2026-09-14 001` was meant to close, reopened by a
filesystem whose clock is too blunt to support the claim.

### The fix: say so, in the spelling HTTP already has

HTTP has a word for "this tag may not change when the content does" — a **weak
validator**, `W/"…"` — and it already has the two rules this needs:

- a weak validator may not be used to assemble a range, and
- `If-Match` compares **strongly**, so a weak tag never satisfies one.

So the fix is to emit the truth rather than to invent a mechanism.
`Granularity::of(path)` asks `statfs` once per root (`MSDOS_SUPER_MAGIC`, or
exFAT's `0x2011BAB0`, which `nix` has no constant for); `validator_of(meta,
granularity)` marks a tag weak **only while the file is younger than one
tick**. Once two seconds have passed, no later write can land on the same
second, and the tag is as good as it is anywhere else — so the weak spelling
costs nothing in the steady state, which is what makes it acceptable to serve
at all.

Three call sites changed in step, and all three are the same idea:

| Where | Before | After |
| --- | --- | --- |
| `If-Match` on `GET` | string compare | `strongly_matches` — refuses inside the tick |
| `If-Range` | string compare | `strongly_matches` — Chrome restarts, does not stitch |
| `If-None-Match` → `304` | string compare | skipped entirely when weak |
| `Precondition::Exactly` (`PUT`, `DELETE`) | `etag_of(meta) == want` | `strongly_matches` |

The last one matters as much as the download: a stale `If-Match` on `DELETE`
inside the tick would have deleted a file other than the one the caller read.

## 2. The 2 GiB free-space floor made most USB sticks read-only

`free_space_floor_bytes` defaults to 2 GiB and was applied to *the destination
filesystem*. Point the file manager at a 512 MiB stick and **every** write is
`507 insufficient_storage`, whatever is on it — there is no state in which a
512 MiB drive has 2 GiB free.

The floor exists for one reason: **a kiosk whose own disk fills up is a session
that will not start.** A removable drive filling up costs nobody an evening.
So the floor now applies only when the destination is on the same device as
shepherdd's home (`FileService::floor_applies`, comparing `st_dev`), and an
unknown device keeps the floor, which is the cautious direction and the old
behaviour.

## 3. Every refusal the drive makes read as a fault in the device

`FileError::from_io` mapped `NotFound`/`PermissionDenied`/`AlreadyExists` and
`ENOSPC`, and sent everything else to `500 internal`. On a FAT drive
"everything else" is most of what happens:

| errno | What the drive is saying | Was | Now |
| --- | --- | --- | --- |
| `EINVAL` (22) | that name has a `:` `*` `?` `"` `<` `>` `|` or `\` in it | `500` | `400` naming the characters |
| `EFBIG` (27) | past FAT32's 4 GiB per-file ceiling | `500` | `413` |
| `EROFS` (30) | mounted read-only, usually a dirty bit after a yank | `500` | `403` |
| `ENAMETOOLONG` (36) | longer than the format's name field | `500` | `400` |

A `500` tells a person their device is broken. A `400` tells them to rename the
file, which is the true and actionable thing. The errno arms now run *before*
the `ErrorKind` arms, because `ErrorKind` either flattens these into something
misleading or leaves them uncategorised depending on the Rust version.

One related change: the temp file in `stream_to_temp` was reported as
`"a temporary file"`. The temp file is this API's business; a person who typed
a name the drive will not take should be told about *that* name.

## What this says about the test suite

The 49 route tests are honest about what they cover, which is the protocol.
They ran — and pass — on a filesystem that can keep every promise the code
makes. Every one of these three bugs lives in the gap between "the protocol is
right" and "the storage under it behaves like the protocol assumes", and no
amount of tmpfs testing reaches into that gap.

Hence the next chapter: a FAT image in the integration-test scripts, so this is
a thing that can be re-run rather than a thing that was once done.

## The harness

Two loopback images, because the two properties that matter cannot be arranged
any other way on every machine: a drive **smaller than the free-space floor**
and a drive **mounted read-only**.

- `scripts/integration-tests/setup-removable-dev.sh` makes two 512 MiB FAT32
  images and mounts them at `/media/shepherd-fat` and
  `/media/shepherd-fat-ro`, the second `ro`. `--teardown` undoes it. Nothing
  survives a reboot, deliberately.
- `scripts/integration-tests/test-removable.sh` checks the mounts and runs the
  tests, following `test-firewall.sh`.
- `crates/shepherd-http/tests/files_removable.rs` — eight `#[ignore]`d tests
  that print `[SKIP]` and pass when the mounts are absent, so
  `cargo test --include-ignored` is the same command on CI.
- `dosfstools` and `util-linux` in `scripts/deps/test.pkgs`.

One check in there is worth naming: the skip test reads `/proc/mounts` and
requires the fstype to be `vfat`/`exfat`/`msdos`, rather than just checking that
the directory exists. An **unmounted** `/media/shepherd-fat` is an ordinary ext4
directory that would pass every assertion in the file for the wrong reason —
which is the precise failure this whole exercise exists to stop.

`roots::removable` was also split in two: `removable_mounts(mounts, uuids)` is
now a pure function of the two files it reads, and the cases that cannot be
arranged on a test machine — a label with a space, a filesystem with no UUID,
two sticks both labelled `UNTITLED`, a torn `/proc/mounts` line — are unit
tests. The privileged half is what `files_removable.rs` covers.

## What the harness found on its first run

Two more, neither of which the fixes above had reached.

### 4. The *upload* response still handed out a strong tag

`GET` had learned to spell a coarse tag weakly. `PUT` had not: `finish_write`
and `finish_chunked` both built theirs with `etag_of`, so the `201` answering
an upload to a FAT drive carried `ETag: "13-…"` — and the obvious thing for a
client to do with it is to send it straight back as an `If-Match`. Both now
return a `Validator` and emit `validator.header()`.

The lesson is narrow and worth writing down: *a validator is a property of the
file, not of the route*, and fixing it at one route is fixing it nowhere.

### 5. `412` was a lie in the one case the drive creates

With the above fixed, a delete issued within two seconds of the upload before
it answers `412` — correctly, because the tag genuinely cannot distinguish the
file from a replacement. But the message said "that file has changed since you
last read it" about a file nobody had touched.

`Precondition::check` now separates the two: when the caller's tag *equals* a
weak one, the answer says the drive records times too coarsely to be sure, and
to try again in a moment. Which is true, and actionable, because the tick
passes on its own.

## Deliberately not tested against the image

- **`ENAMETOOLONG`** is unreachable through this API. `resolve` caps a path
  component at 255 **bytes** and vfat's limit is 255 **UTF-16 units**; any name
  over vfat's limit is over 255 bytes too, so the API's own `400` always lands
  first.
- **`EFBIG`** needs a file over 4 GiB, and the image is 512 MiB, so `ENOSPC`
  arrives first. A 4 GiB image would make the suite cost minutes.

Both mappings are covered by the unit test on `from_io` instead. This is
written down because a test that appears to cover them and does not is worse
than no test.

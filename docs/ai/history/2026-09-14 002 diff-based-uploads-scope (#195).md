# Diff-based uploads, rsync-style (issue #195) — scope

> Status: **scoped, not built.** The survey and the argument, not an
> implementation. Written before the directory-upload work, because it changes
> what that work should do.

## Prompt

> hm I think the biggest omission before starting that work is diff-based
> uploads a la rsync. scope out the protocol changes that would be needed to
> support this

## What "rsync-like" means here, in three levels

rsync is usually remembered for its delta algorithm, but that is not what it
does most of the time. By default it compares size and mtime, skips the files
that match, and sends the rest whole. The delta algorithm only runs for files
it decides to transfer *and* only with `--checksum`-ish conditions. Separating
those levels is most of this document, because they differ by an order of
magnitude in cost and by two in value.

| | What it does | Cost | Value here |
| --- | --- | --- | --- |
| **1. Skip unchanged files** | size + mtime quick check, per file | small | **large** |
| **2. Skip unchanged *parts*** | fixed-size chunk digests, copy the rest from the target | moderate | small but real |
| **3. True rsync deltas** | rolling weak hash + strong hash, byte-aligned matching | large | ~nil |

**Level 3 is not worth building for this device.** The rolling hash exists to
find matches at arbitrary byte offsets, which matters when a file is *edited in
place* — a mail spool, a database, a VM image. What this file manager carries
is books, ROMs, videos, posters and the odd `movies.toml`: content that is
replaced wholesale or not at all. A rolling-hash implementation in TypeScript,
plus its server half, is one to two weeks for a case the workload does not
have. Level 2 catches the realistic version of the same win (a file whose first
chunk changed) for a fraction of the effort.

## Two measurements that decide the shape

Taken here rather than assumed, because the whole design turns on whether
hashing is affordable:

- **Hashing on the client is free.** `crypto.subtle.digest('SHA-256')` over
  8 MiB slices runs at **~1.9 GB/s** on this machine — a 4 GB file hashes in
  about two seconds of CPU. The client's own disk read will dominate, and both
  are an order of magnitude faster than sending the bytes over wifi.
- **Hashing on the device is not free.** `sha256sum` reads and hashes at
  **~560 MB/s** on this dev box, which is a fast VM disk; a kiosk on an SD card
  or an old SATA SSD will be a fraction of that. Hashing one file on demand is
  fine. Hashing a 40 GB library on every sync is five to thirteen minutes of
  the device's disk, and that is before anything is uploaded.

So: **the client may hash freely; the device must be asked for digests
sparingly, and only for files the quick check could not settle.**

## The blocker nobody would guess

**An upload destroys the only thing a quick check could compare.** The device
stamps the file with its own write time, so after uploading `film.mp4` the
device's mtime is "when it landed", not "when the film was made". Re-run the
same upload tomorrow and every file looks changed: same size, different mtime,
nothing skippable.

rsync does not have this problem because it preserves mtimes by default
(`-t`, implied by `-a`) — and that is exactly why its quick check works at all.

This is one header and one `utimensat`, and without it **none of the rest is
worth building**. It also has two side effects worth wanting:

- The etag is `size-mtime`, so preserving the source mtime makes the etag
  *stable across re-uploads of identical content* — which the diff can then use
  as a cheap content identity.
- The Files tab's **Modified** column starts meaning "when this was made"
  rather than "when it was copied here", which is what a person expects of a
  copied file.

## The protocol changes, ranked

### 1. Preserve the source mtime — `X-Source-Modified`

```
PUT /api/v1/files/content?root=…&path=…
X-Source-Modified: 1789400724326        (epoch milliseconds)
If-None-Match: *
```

Applied after the rename, on both the one-shot and the resumable path. The
browser has it: `File.lastModified`.

*Effort: a couple of hours, including a test that a re-upload of untouched
bytes produces the same etag.*

### 2. Recursive listing — `GET /files/list?depth=full`

A diff needs the device's whole subtree before it sends anything. Today that is
one request per directory; for a library of fifty folders over a 300 ms link
that is fifteen seconds of round trips before the first byte moves.

```
GET /api/v1/files/list?root=home&path=Books&depth=full&limit=5000&cursor=…
→ { "entries": [ { "path": "covers/hobbit.png", … } ], "truncated": …, "cursor": … }
```

- `path` relative to the requested directory, rather than a bare `name`.
- The same node budget and cursor as the flat listing, because a ROM set with
  50 000 files in it is a real thing.
- **Does not descend into symlinked directories at all.** Escaping links are
  already refused; refusing to *recurse* through any of them is what keeps a
  loop from being expressible, and is simpler than tracking visited inodes.
- The denied-subtree checks apply per node, as now.

*Effort: ~1 day server, with the walk budget and its tests.*

### 3. Digests on demand — `GET /files/digest`

For the files the quick check cannot settle (size matches, mtime does not, or
the client wants certainty), and for detecting renames.

```
GET /api/v1/files/digest?root=…&path=…&algo=sha256&chunk=8388608
→ { "size": 4294967296, "chunk_bytes": 8388608, "whole": "…",
    "chunks": ["…", "…", …] }
```

- **Bounded**: a maximum number of bytes hashed per request, and a `412` if the
  file changed while it was being read (the etag it was started against).
- **Cached** by `(path, size, mtime)` in memory, so asking twice is free and a
  digest survives the plan-then-upload gap.
- `chunk` omitted means whole-file only, which is the cheap question.

*Effort: ~1 day, mostly the bound and the cache.*

### 4. Assemble from what is already there — `X-Copy-From: target`

This is level 2, and it composes with the resumable upload rather than
replacing it. A chunk the client knows already matches is not sent:

```
PUT /api/v1/files/content?root=…&path=…&upload=<token>
Content-Range: bytes 8388608-16777215/4294967296
If-Match: "<etag the digests were taken against>"
X-Copy-From: target                      (no body)
```

The device appends that byte range *of the existing target file* to the part
file. Everything else — the offset check, the `409` re-sync, the rename, the
precondition at the end — is already built and unchanged.

Note the `If-Match`: without it, a target edited between the digest and the
upload would be spliced into the new file. The route honours `If-Match` on
`GET` since the browser-resume work, and this is the same idea applied to a
write.

*Effort: ~1.5 days, including the case where the target is replaced mid-upload.*

### 5. Not needed: renames and deletions

Both fall out of what exists.

- **Renames** are `POST /files/move`, which is already there. The client spots
  them: a local file with no counterpart by path, whose size and digest match a
  device file that has no local counterpart, is a move rather than an upload
  plus a delete. For a re-organised library this is the difference between
  moving 40 GB and sending it again.
- **`--delete`** is `DELETE /files/entry`. It needs a confirmation and an
  explicit opt-in in the UI, not a protocol change — and it should never be the
  default, because "the folder I dragged is the truth" is a dangerous thing to
  assume about a device a child also uses.

### 6. Optional: batch `mkdir`

`POST /files/dir` taking `paths: [...]` instead of one `path`, so a deep tree
costs one round trip rather than one per directory. Minor, and only worth it
once directory upload exists.

## The zero-cost baseline, for comparison

Worth stating because it may be enough: **a client can already skip files that
exist, with no protocol change at all.** Upload every file with
`If-None-Match: *` and treat the `412` as "already there". That gives
"don't re-send what is present" today, for a one-line change — it just cannot
tell *changed* from *unchanged*, so an edited file is never re-sent either,
which is the wrong failure.

The quick check (changes 1 + 2) is what turns that into a real sync.

## Phases

| Phase | Contents | Effort |
| --- | --- | --- |
| **A** | `X-Source-Modified`, recursive listing, client-side quick check (size + mtime, with tolerance) | ~3 days |
| **B** | `GET /files/digest`, verify-on-demand, rename detection | ~2 days |
| **C** | `X-Copy-From: target` and chunk-level skipping | ~2 days |
| — | True rsync rolling-hash deltas | 1–2 weeks, **recommend against** |

Phase A alone gives rsync's everyday behaviour: re-drop a library and only the
new files move. It is also the phase directory upload should be built *on top
of*, rather than before — which is why this was worth scoping first.

## Hazards, each of which has bitten somebody

- **FAT mtime granularity.** Removable drives are a first-class root here, and
  FAT/exFAT store mtimes at 2-second resolution (and in local time). A quick
  check comparing exact milliseconds will report *every* file on a USB stick as
  changed. rsync has `--modify-window` for exactly this; the tolerance must be
  ≥2 s, and the phase-A work should test against a real FAT-formatted image.
- **Amplification.** Recursive listing and digests both let one small request
  cost a lot of the device's disk. Both need hard bounds, and the fact that the
  caller is already an administrator is not a reason to skip them — a stuck
  client retrying a 40 GB digest is a denial of the child's evening.
- **Etag forgeability.** Once a client can set mtime, it can make a file's etag
  equal a previous one, which weakens `If-Match` as a concurrency check between
  two administrators. Both are administrators, so this is a note rather than a
  hole — but it should be written down where the etag is defined.
- **Clock skew becomes irrelevant, and that is the point.** After change 1 the
  comparison is source-mtime against source-mtime; the device's own clock never
  enters it. Worth stating, because the obvious alternative — comparing against
  the device's write time — silently breaks on a device whose clock is wrong,
  which is most of them at first boot.
- **A digest is a promise about a moment.** Between planning and uploading,
  anything on the device may change. Every write derived from a digest must
  carry the etag it was computed against, or the diff becomes a way to corrupt
  a file rather than to save bandwidth.

## Open questions

1. **Is Phase A enough?** For "add three videos to a library of two hundred" it
   is the whole win. Phases B and C only pay when files are *modified* rather
   than added, which may never happen on this device.
2. **Should the quick check trust mtime, or verify with a digest?** rsync
   trusts it by default and offers `--checksum` for the paranoid. Given how
   cheap client-side hashing turned out to be, "verify anything whose size
   matches but whose mtime differs" is affordable and closes the one case where
   mtime lies.
3. **Does the device ever become the source?** Everything above assumes the
   browser pushes. A future "copy this folder off the device" would want the
   same digests in the other direction, which is another reason to keep
   `GET /files/digest` symmetric and not bolt it onto the upload path.

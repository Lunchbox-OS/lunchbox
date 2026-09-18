# Uploads that survive a bad link (issue #195)

> Status: **built**, 2026-09-13. Follows the three file-manager stages in
> `2026-09-11 003…005`; this is the resilience work the review of them asked
> for.

## Prompt

> how resilient is the protocol to poor network conditions -- as might be the
> case on the wifi chip on repurposed hardware? I'm noticing there's no support
> for partial/resumable uploads

Then, after the survey below: *"do both here"* — both the cheap fixes and
chunked resumable uploads, on the same branch.

## What the survey found

Measured on a device rather than reasoned about, because one of the answers
contradicted the guess.

**Already sound**, and worth writing down so nobody re-solves it:

- **Nothing half-written ever appears at the destination.** Temp file, `fsync`,
  rename. A child mid-book never opens a truncated file.
- **The partial is cleaned up when the client dies.** Killing the uploader
  mid-flight three times left no stray file each time: the body stream errors,
  and the handler's error path unlinks it. (A first, sloppier test suggested a
  60 MB leak; the kill had missed and the upload had actually completed. The
  24h sweep remains the backstop for the case the error path cannot cover — a
  *daemon* killed mid-write. It is opportunistic rather than timed: see the
  correction in `2026-09-18 001`.)
- **Retrying is safe by construction.** `If-None-Match: *` means an upload
  retried after an ambiguous failure cannot silently clobber.
- **Browsing survives drops**: listings are React Query queries, which retry
  three times with backoff by default.
- **Downloads were already resumable** — `Accept-Ranges`, `ETag`, `206` — and
  on the same-origin path the browser's own download manager does the resuming.

**Not sound**, and the reason for this work:

1. **Uploads were one request, all-or-nothing.** A drop at 95% of a 4 GiB video
   cost 4 GiB. On hardware that drops every few minutes, a large file might
   never land at all — no amount of retrying fixes that without resumption.
2. **No retry, manual or automatic.** The tray offered Replace and Cancel; an
   errored transfer just sat there.
3. **No timeouts anywhere** — not in axios, not in the daemon. A stalled but
   open connection froze the progress bar until TCP gave up, about fifteen
   minutes, with nothing on screen saying so.
4. **`If-Range` was ignored on download**, so a browser resuming a download of
   a file that changed in between would stitch two versions together.

## What was built

### The wire

One new query parameter and one new route, both additive: the whole-file `PUT`
is untouched, so `curl`, the e2e harness and every existing test still work.

```
PUT    /files/content?…&upload=<token>   Content-Range: bytes X-Y/Z
GET    /files/upload?…&upload=<token>    → {"offset": N}
DELETE /files/upload?…&upload=<token>    → 204
```

**The state is the part file.** No session table, nothing to expire, and a
daemon restart loses only the chunk that was in flight. It is the same
`.name.token.part` mechanism the one-shot path already used for atomicity —
which is why this was a smaller change than it sounds.

**The token is a filename component**, so it is validated like every other
caller-supplied name here (8–64 of `[A-Za-z0-9_-]`), and there is a test that
tries to escape with it.

**The precondition is checked twice**, and the second one is the one that
matters: at the first chunk so a refusal is cheap, and again at the rename, so
a file that appeared while a 4 GiB upload was in flight is not overwritten by
it. There is a test for exactly that race.

### The client

- Anything over one chunk (8 MiB) goes up in pieces. The unit of retry is a
  chunk, not a file.
- **Every recovery asks the device where it got to** rather than assuming.
  This is the detail the device test taught: a connection that dies mid-chunk
  leaves a *partial* chunk on the device, so resuming from the chunk boundary
  would re-send bytes it already has. Re-syncing first is strictly less work on
  exactly the link that cannot afford it.
- Transient failures retry three times with backoff, and the counter resets on
  every success — three failures in a row is a link that is down; three
  failures across four gigabytes is a Tuesday.
- **A stall is a failure.** A watchdog aborts any request that has not moved a
  byte in 30 seconds, which turns the fifteen-minute freeze into a retry.
- **Retry in the tray** resumes rather than restarts, and the tray says
  "resumed from 1.2 GB" when it does — because "it started again from zero" is
  what a person watching a slow link is afraid of.
- A cancelled transfer `DELETE`s its part file: a cancel that leaves gigabytes
  on a small disk until tomorrow is not a cancel.
- The token is derived from the file's name, size and modification time, so
  re-adding the same file after a page reload resumes what is on the device.

### The one thing the tests changed

`isTransient` first treated every 5xx as worth retrying. Writing the test made
it obvious that `507 Insufficient Storage` is a *decision* — the disk is full,
and re-sending gigabytes will not change that — so only `500`, `502`, `503` and
`504` retry now.

## Verified on the device

- **A 30 MB upload, with the connection killed twice mid-chunk**, resumed from
  the device's own offset each time (2,097,152 and 12,582,912 — neither a chunk
  boundary) and landed byte-identical to the original, with no part file left.
- The same file through the **UI**: four chunks, landed identical, no strays.
- `DELETE /files/upload` took the 12 MB part file with it; the offset went back
  to 0.
- A token of `../escape` was refused with `400`.
- The finished file is still protected: re-sending chunk 0 with
  `If-None-Match: *` after the file landed answers `412`.

## Still not done, deliberately

- **Resuming across a page reload needs the person to re-add the file.** The
  browser cannot re-read a `File` handle it was given in a previous session.
  The token makes the *device* side resumable; the missing half is the File
  System Access API, which is Chromium-only.
- **No server-side inactivity timeout.** The client's watchdog covers the case
  that matters (a person watching a frozen bar); a stalled connection the
  client has forgotten about still holds a part file until the sweep reaches
  that folder — which happens when somebody uploads into it or opens it, not on
  a clock. See `2026-09-18 001`.
- **Downloads resume only as well as the browser does.** The cross-origin path
  (`apiBase` pointing at another device) still buffers to a blob with no
  resumption — it is the path that needs it least, and fixing it means
  streaming through the Service Worker or the File System Access API.

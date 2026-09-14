# What a browser actually sends to resume a download (issue #195)

> Status: **investigated and fixed**, 2026-09-14. A follow-up to
> `2026-09-13 002`, which made *uploads* survive a bad link and claimed
> downloads were already fine.

## Prompt

> What is the behavior for interrupted *downloads*? I'm particularly curious
> about whether the resume flow from Chrome and Firefox work correctly

## The short answer

Firefox's resume works, and it now works *safely* — but only because this
question was asked. The previous change had added `If-Range` on the strength of
how the specification tells the resumption story. **Firefox does not send
`If-Range`. It sends `If-Match`**, which the download route ignored entirely,
and ignoring it was a silent-corruption bug.

## How it was measured

Guessing what a browser sends is how the bug got there in the first place, so
this time the browser was asked. A small logging proxy sat in front of the
daemon (`scratchpad/proxy.py` in the session's scratch), injecting the machine
token so the browser needed no session of its own, recording the headers of
every request, and cutting the connection part-way through the first response
body — which is what a wifi drop looks like from the browser's side.

Firefox was driven through geckodriver, with the download resumed through the
privileged `Downloads` API rather than by clicking in `about:downloads`.

Two harness notes for whoever does this next:

- **`Navigate To` never returns for a download**, because a download fires no
  load event; it blocks until the page-load timeout. Start it with
  `window.location.href = …` from a script instead.
- **Chrome context needs `-remote-allow-system-access`**, which Firefox 155
  refuses to accept through capabilities. Launch Firefox yourself with
  `MOZ_HEADLESS=1 firefox -marionette -remote-allow-system-access -profile …`
  and attach with `geckodriver --connect-existing --marionette-port 2828`.
  (`-headless` as an *argument* is not enough: the snap build shuts down with
  "Wayland compositor unavailable" unless `MOZ_HEADLESS` is in the
  environment.)

## What Firefox sends

```
#1  GET …/files/content?root=extra-0&path=film.bin
    → 200, Content-Length: 40000000, ETag: "40000000-1789400724326124000"
    → connection cut at 8,000,000 bytes

#2  GET …/files/content?root=extra-0&path=film.bin
    Range: bytes=8000000-
    If-Match: "40000000-1789400724326124000"
    → 206, Content-Range: bytes 8000000-39999999/40000000
```

`If-Match`, not `If-Range`. Firefox kept an 8 MB `.part` file and reported
`hasPartialData: true`, so it had decided the response was resumable from the
`ETag` and `Accept-Ranges` alone — no `Last-Modified` needed.

## The bug that found

`If-Match` on a `GET` means "serve this only if it is still the same
representation"; a mismatch must be `412`. The route did not look at the header
at all, so a resume whose file had changed in the meantime was answered with
`206` and **bytes from the new file at the old offset**.

Demonstrated rather than argued: 8 MB of the original file, the file replaced
on the device, then Firefox's exact resume request. The result was a 40 MB file
that matched *neither* version — and Firefox would have reported it as a
successful download.

The fix is eleven lines: honour `If-Match` on the download route, before the
range is considered, answering `412` when it does not match. With it, the same
scenario ends with Firefox reporting `NS_ERROR_ENTITY_CHANGED`, discarding the
partial data, and leaving nothing on disk.

## Every resume shape, measured

Against the fixed route, with a 40 MB file:

| Request | Answer |
| --- | --- |
| `Range` only (`curl -C -`, `wget`) | `206` |
| `Range` + matching `If-Range` (Chromium's shape) | `206` |
| `Range` + stale `If-Range` | `200`, whole file |
| `Range` + matching `If-Match` (Firefox's shape) | `206` |
| `Range` + stale `If-Match` | `412` |
| `Range` + `If-Match: *` | `206` |
| `Range` + `If-Range` as a date | `200`, whole file |

End to end: Firefox's resumed 40 MB file is byte-identical to the one on the
device, and so is `curl -C -`'s.

## What was *not* verified directly

**Chrome was not run.** It is not installed on this machine and there is no
package of it in the dev dependencies. What is known: Chromium's download
resumption sends `Range` plus `If-Range` with the stored strong `ETag`
(`If-Range` is the header its download code uses), and both the matching and
stale forms of that request are exercised above and behave correctly. That is
evidence about *our* side of the contract, not about Chrome's, and the
difference is worth stating plainly — this whole investigation exists because
the specification-shaped guess about Firefox was wrong.

If Chrome ever needs verifying here, `deps install` would have to grow a
browser, and the same proxy harness would do the rest.

## Deliberately not changed

**No `Last-Modified`.** Adding it would invite clients to send `If-Range` and
`If-Modified-Since` as *dates*, and the route validates one thing — the
`ETag`. A date form would then have to be compared against the file's mtime at
second resolution, which is a second validator to keep in step with the first.
Both browsers preferred the `ETag` when offered one, and Firefox was happy to
resume without a `Last-Modified` at all, so the single validator stays.

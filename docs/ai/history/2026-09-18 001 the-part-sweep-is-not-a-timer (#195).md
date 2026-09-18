# The `.part` sweep is not a timer (issue #195)

> Status: **corrected and extended**, 2026-09-18. A correction to
> `2026-09-13 002`, which called the sweep a backstop in language that implied
> a clock.

## Prompt

> how deep does the sweep go

## The answer, and why it was worth asking

**One directory. Not recursive.** `sweep_stale_parts` is a single `read_dir`
with no descent, which is enough, because both part-naming schemes put the file
beside its target:

```
.film.bin.4242-1789400724326124000.part   one-shot
.film.bin.u-abc12345.part                 resumable
```

The depth was never the interesting part. The trigger was. There were two call
sites and neither was a timer:

- `prepare_chunk`, when `range.start == 0` — the first chunk of a new
  resumable upload.
- `finish_write`, after a **successful** one-shot upload.

Both sweep the directory the upload is landing in. So a part file was collected
only when *another upload happened in that same folder*, a day or more later.
Give up on a 4 GiB film into `~/Films/2019/`, never upload there again, and the
bytes stayed for good.

Four places in the documentation said otherwise — "the sweep would collect it a
day later", "holds a part file until the 24h sweep" — which is true only under
a condition none of them mentioned. That wording was written during the
resumable-upload work and was wrong when written.

## What was changed

**Listing a directory now sweeps it**, before the listing is built so a
collected file is not also reported as being there.

That is the other moment a stale part can be noticed, and it is a better one
than an upload: it is the moment somebody is actually *looking* at the folder
the stray bytes are in. Between the two triggers, every folder anybody has a
reason to care about is covered.

It is a `GET` with a side effect, which is worth saying out loud rather than
slipping in. The alternative — a daemon-wide daily walk — was rejected: every
root here holds directories with tens of thousands of files in them, and
scanning those on a schedule is the child's evening rather than housekeeping.
This is the same amplification argument as in `2026-09-14 002`.

**The cost is one extra `getdents` pass, and that is genuinely noise.** It
looks like a second walk of the directory and is not: the sweep `stat`s only
entries whose names match `.*.part`, which is approximately none of them, while
the listing beside it `stat`s every single entry. Against a ROM set the
listing's own work dominates by orders of magnitude.

**Read-only roots are skipped.** There is nothing to delete with and no
permission to do it, so the sweep is not attempted at all.

## What is still true, and now written down where it is decided

A transfer abandoned into a folder that is never uploaded to *or opened* again
keeps its part file. There is no timer anywhere in this feature.

That is acceptable because the file is **dotted**, so a listing reports it as a
hidden entry rather than concealing it: a person who turns hidden files on sees
it, is told its size, and can delete it. The failure mode is "some space is
used until somebody looks", not "space disappears with no way to find it".

`DELETE /files/upload` remains the deliberate way to give bytes back, and the
tray's cancel uses it, because a cancel that leaves gigabytes on a small disk
until somebody happens to browse that folder is not a cancel.

## Tests

- `opening_a_folder_collects_what_was_abandoned_in_it` — a day-old part in a
  subfolder, and a listing of the *parent* that must not reach it (the depth,
  asserted rather than assumed), then a listing of the folder itself, which
  must collect it and must not name it in the response. A fresh part beside it
  survives and is listed as hidden.
- `a_read_only_place_is_listed_without_trying_to_tidy_it`.
- The upload-side sweep keeps its own test from `2026-09-14 004`.

Both new tests were checked against the unmodified handler and fail there.

# What really makes a non-UTF-8 filename (issue #195)

> Status: **investigated; one bug found and fixed, one gap left open**,
> 2026-09-18.

## Prompt

> I see a test `a_name_that_is_not_utf8_is_shown_but_cannot_be_addressed`: what
> circumstances would actually produce such a file

A fair challenge: the test's comment said "a camera, an old archive, or a
program written before anyone cared", which is hand-waving. Measured instead.

## The answer: this project's own removable drives

Linux filenames are byte strings — anything but `/` and NUL — so nothing
enforces UTF-8 anywhere. But the mechanism that matters for *this* device is
not exotic software. It is **vfat mounted with a non-UTF-8 `iocharset`**, which
is the kernel default and exactly what `setup-removable-dev.sh` produces:

```
/dev/loop11 /media/shepherd-fat vfat ...,codepage=437,iocharset=iso8859-1,...
```

A stick written on Windows stores `café.mp3` as UTF-16. Read back through
`iocharset=iso8859-1`, the `é` arrives as the single byte `0xE9`:

```
created through iocharset=utf8 :  café.mp3
read back with the default     :  b'caf\xe9.mp3'   *** not valid UTF-8 ***
```

`exfat` is safe — it always converts from UTF-16 to UTF-8. `vfat` is the
exposure, and `roots.rs` already notes that a sway kiosk often has no
automounter, so a drive is as likely mounted by `fstab` or by hand as by
`udisks2` (which would pass `utf8=1`). That is precisely when the kernel
default applies.

Second most likely: **extracting a ZIP**. The format stores names in an OEM
codepage unless bit 11 says UTF-8, and Info-ZIP's `unzip` copies those bytes
out verbatim — so legacy CP932/CP936/CP1251 archives, which is what ROM sets
and scanned comics arrive as, produce them directly. Third: any activity at the
kiosk uid can simply create one.

So the test's premise is sound, and understated: the likeliest cause is the
drive the feature was built for.

## The worse sibling, which has no flag

Names that iso8859-1 cannot represent *at all* do not become invalid UTF-8.
They become literal question marks:

```
中文字.zip   한국어.zip   日本語.zip    →    ???.zip   ???.zip   ???.zip
```

Three distinct files, one visible name, and both survivors `lstat` to the same
inode. This is valid UTF-8, so `name_not_utf8` does not fire; the listing shows
identical rows with identical etags, and a delete removes whichever the kernel
resolves. There is no name that distinguishes them, so there is nothing the API
can do about it — but it is worth knowing it can happen.

## The bug that fell out of it

Deleting one of the colliding files made the other two **disappear from the
listing** while `readdir` still reported them. The delete was correct (the disk
went 4 entries to 3); the listing was wrong, and stable across repeats until
the drive was remounted.

The cause was in `list_dir`, and it was not specific to FAT at all:

```rust
for entry in entries.flatten() {          // readdir errors: discarded
    let Ok(link_meta) = entry.metadata() else {
        continue;                          // stat failures: discarded
    };
```

A rendering with `?` in it is not a name the filesystem can look up again, so
`fstatat` fails and the entry was dropped. The same happens whenever another
writer unlinks something between the directory being read and the row being
built, and whenever a drive answers `EIO` for one entry — which a failing USB
stick does.

**A list that is quietly shorter than the folder is the one answer a file
manager must never give.** So:

- An entry that cannot be explained is now a row — named, empty columns,
  `unusable: "unreadable"`. It is ordered *before* the `special_file` arm,
  because `kind` is `Other` there only for want of information, and claiming "a
  socket, a pipe or a device" would assert something nobody established.
- It stays **deletable**. Not knowing what a thing is says nothing about
  whether its name still reaches it, and being unable to tidy up after a bad
  drive is the worse answer.
- The one thing that cannot become a row — an entry the directory could not
  even name — is counted in `Listing::unreadable`, omitted from the JSON when
  zero, and drawn as a row offering "try again".

### Making it deterministic

A directory that is **readable but not searchable** (mode `0444`) is the clean
way to arrange it: `readdir` returns every name and every `fstatat` is
`EACCES`. The test skips when running as root, which bypasses the permission it
is built on. Against the old code the listing came back empty.

## Left open: an un-nameable file cannot be deleted

Measured against a real mount, every spelling fails:

```
DELETE path=caf%EF%BF%BD.mp3   (the lossy name, as the UI sends it)  → 404
DELETE path=caf%E9.mp3         (the raw byte, percent-encoded)       → 404
DELETE path=café.mp3           (what it was called when written)     → 404
```

This is inherent: paths are `&str` and the API never accepts raw bytes. But the
consequence deserves stating, because it contradicts the principle the rest of
this feature follows — an escaping symlink is listed *precisely so somebody can
delete it*. A `name_not_utf8` entry can be seen, is flagged, and can never be
removed, on a device whose whole premise is that there is no shell.

**Since closed.** The listing hands back a `handle` on exactly those rows — the
entry's own bytes in hex — and `DELETE` takes it in place of the last path
component, with `path` naming the containing folder.

Hex rather than base64 (no dependency) and rather than percent-encoding, which
*cannot* work: a query string is decoded to a `String` before any handler sees
it, so a percent-escape for a byte that is not valid UTF-8 fails to parse — the
exact situation the handle exists for.

It is a **name, not a path**, and that is the whole of its safety. The folder it
applies to goes through the resolver like any other request; the handle may only
add one component to it, and is refused if it contains a separator or a NUL, is
`.` or `..`, is empty, is not hex, or is longer than a name can be. A test
table fires all ten of those at it and checks that a file outside the root is
still there afterwards.

`POST /files/move` takes one as `from_handle`, and it is the better half of the
two. Deleting an un-typable file throws it away; **renaming it to something
typable is the repair** — the bytes stay, and every other route works on it
again afterwards. One function resolves the handle for both routes, so the
validation cannot drift between them, and the forgery table is fired at both.

There is deliberately **no handle for a move's `to`**. A destination is always
something the caller typed, and a move that could name an unreachable target
would be a way to *create* files nothing can reach — the opposite of the point.
Nothing this API does can put a non-UTF-8 name on a disk, and that is worth
keeping.

The consequence has to be lived with rather than designed away: **moving such a
file to another folder renames it**, to the lossy rendering, because that is the
only thing a `to` can say. The bytes survive and the name does not — on the
drive it came from, the `é` in `café.mp3` becomes three bytes of nonsense. Since
the old and new names *render identically*, nothing about the outcome would show
a person what happened, so the UI asks first and says what is lost. Renaming it
yourself first is the way to choose the new name, and the confirmation says so.

A subtlety found while wiring this up: the drag gesture and the **Move to…**
menu item are both gated on `canRename`, so allowing rename by handle silently
enabled two affordances that had no handle plumbed through them and answered
`404`. Fixed in the same breath; worth noting because the gate is shared and
the next person to widen one of these will widen all three.

Download does not take one either. A file that cannot be named has no
`Content-Disposition` to offer, and not being able to read it was never the gap;
not being able to fix or remove it was.

One limit, deliberately: **this does not separate the `???.zip` collision.**
Three files whose rendered names are byte-identical produce three identical
handles, because a handle *is* the rendered bytes. No API can invent a
distinction the filesystem will not make; the fix for that case is mounting the
drive with a charset that can spell its contents.

## Not chased to the end

The listing fix explains and resolves what was observed, and the test proves
the mechanism. What was *not* established is why the two survivors' `fstatat`
succeeded from a separate process moments later — the failure reproduced
reliably in the daemon and never outside it. Ruled out along the way: the
`.part` sweep added in `2026-09-18 001` (removing it changes nothing), and any
in-process `readdir` caching (`readdir` + `lstat` in one process, unlinking in
between, sees every survivor). Written down rather than smoothed over, because
the next person to see a short listing on a FAT drive should know this corner
was not fully mapped.

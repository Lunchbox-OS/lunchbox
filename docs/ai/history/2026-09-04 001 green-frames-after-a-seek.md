# Green, blocky video after a seek (#163) — investigation

> Status: **root-caused and fixed**, on `feat/159-sponsorblock`. The cause is a
> VA-API DMABUF-export bug in the driver, not in shepherd; the fix is to stop
> asking for the zero-copy path on Linux. Everything below was measured on the
> dev/kiosk machine (`leibniz`, Intel HD 4000 / Ivy Bridge, i965 VA driver, Mesa
> crocus) in the headless dev session with `--gpu`.

## Prompt

> #163 is currently checked out. I'm still observing the seek and
> SponsorBlock-related video artifacts that should have been addressed in this
> branch -- investigate
>
> for context: when seeking or the SponsorBlock integration seeks on the user's
> behalf, the video goes green and blocky, almost as if it skipped an iframe and
> never recovered

The branch already carried a fix attempt — commit `0edda68`, "decode cleanly
across a seek, and land exactly on the target" — which set
`hr-seek-framedrop=no` and changed seeks to `absolute+exact`. Its own commit
message called the first half "a diagnosis, not a confirmed fix", because the
session that wrote it believed the machine had no GPU to test against.

**It does.** `/dev/dri/renderD128` is there; what is missing is an ACL entry for
`shepherd-admin`, because logind grants the render node to the *active seat*
session and an SSH/agent shell has no seat. `sudo setfacl -m u:shepherd-admin:rw
/dev/dri/renderD128` is enough to exercise the hardware path, and the whole
investigation below depends on that one line. (`vainfo` is not installed;
`ffmpeg -init_hw_device vaapi=va:/dev/dri/renderD128` is a fine substitute for
checking access.)

## What it actually is

Not the seek, and not shepherd's code.

Bare `mpv`, no shepherd binary involved, reproduces it:

```sh
mpv --no-config --fs --ao=null --vo=gpu --hwdec=auto-safe \
    --input-ipc-server=/run/user/1000/mpv.sock <a 720p file from the video cache>
# ... then, over IPC: {"command":["seek",120.0,"absolute+exact"]}
```

A second or so after the seek the frame turns green and blocky. Luma is roughly
intact — the picture is still legible through it — and the chroma is read from
the wrong place, which is what makes it green: NV12 with `UV = 0` converts to
RGB ≈ `(0, 135, 0)`.

It then *stays* corrupt. Playing on from a corrupt frame does not recover it,
even across keyframes; a later seek sometimes does. That is what "never
recovered" in the report means, and it is also why "it skipped an iframe" is the
wrong model — an error-concealment artifact would clear at the next IDR.

### The measurements

Twelve seeks to fixed targets in a 447 s 720p H.264 file from the kiosk's video
cache, screenshotting the compositor output at +0.4/1.2/2.2/3.2 s after each and
scoring `G − (R+B)/2` over the video area (the source is black and white, so any
green at all is corruption):

| configuration | corrupt seeks |
|---|---|
| `vo=gpu`, `hwdec=auto-safe` → `vaapi` (zero-copy) | **4 of 12** |
| the same, plus `hr-seek-framedrop=no` (the branch's fix) | **4 of 12** |
| the same, plus `hwdec-extra-frames=16` | **4 of 12** |
| the same, plus `hwdec-extra-frames=32` | **4 of 12** |
| the same, plus `vd-lavc-threads=1` | **4 of 12** |
| `vo=gpu-next`, `hwdec=auto-safe` | **4 of 12** |
| `vo=gpu`, `hwdec=vaapi-copy` | **0 of 12** |
| `vo=gpu`, `hwdec=no` (software) | **0 of 12** |

The same four targets fail every time, in every failing configuration — it is
deterministic, not a race.

And in `shepherd-media` itself, driven with `wtype -k Right`, 14 seeks:

| | corrupt seeks |
|---|---|
| `hwdec=auto-safe` (zero-copy, what `main` and this branch shipped) | 2 of 14 |
| `hwdec=vaapi-copy` | **0 of 14** |

### What that rules out

- **`hr-seek-framedrop`.** Identical rates with it on and off. The diagnosis in
  `0edda68` was wrong, and the setting has been reverted; `absolute+exact` stays,
  because it is independently justified (a skip that lands on the preceding
  keyframe would drop playback back inside the span it just skipped) and was
  verified to the millisecond.
- **The video output.** `vo=gpu` and `vo=gpu-next` fail identically, and so does
  `vo=libmpv` through shepherd's own FBO. Nothing about the render API, the
  compositor, or the egui shell is involved.
- **Surface-pool exhaustion.** A pool four and eight times larger changes
  nothing.
- **The decoder.** `vaapi-copy` uses the *same* VA-API decoder on the *same*
  driver and is clean. Only the delivery differs.
- **Playback in general.** Playing the same span linearly, without seeking into
  it, is clean. The seek is the trigger; a seek is just also the thing that
  makes the driver hand over a surface it describes wrongly.

What is left is the DMABUF export — `vaExportSurfaceHandle` on i965, imported as
an EGLImage — describing the chroma plane wrongly for some surfaces after a
flush. shepherd cannot fix that; it can decline to use it.

## The fix

`crates/shepherd-media-core/src/player.rs` now asks for **`auto-copy-safe`**
rather than `auto-safe` on the render-API (Linux) front-end, overridable with
`SHEPHERD_MPV_HWDEC`. Android is untouched: it decodes into a `SurfaceView`
through `mediacodec`, a different path that this does not affect.

This partly reverses issue #115, which moved Linux *to* zero-copy. The cost of
reversing it turns out to be small on the content this device actually plays —
its library is capped at 720p — measured as CPU of the `shepherd-media` process
over 20 s of steady-state playback:

| `hwdec` | CPU (one core) |
|---|---|
| `vaapi` (zero-copy) | 19.1 % |
| `vaapi-copy` | 21.7 % |
| `no` (software) | 55.2 % |

2.6 points of one core, on a four-core machine, to stop showing children a green
screen. (The 24.3 % vs 13.9 % in the #115 write-up was 1080p; nothing in the
cache is 1080p today, because `--max-quality` caps it.)

The `-copy` case in `log_hwdec` used to be a warning — "the zero-copy path is
unavailable" — which is now the normal, deliberate state on Linux. It takes a
`copy_wanted` flag so it still warns when mpv falls back to a readback nobody
asked for, which is the #115 symptom and worth keeping.

### What was deliberately not done

- **Gating on the GPU generation.** Forcing the copy path only for pre-Broadwell
  Intel would keep zero-copy everywhere else, but it hard-codes a guess about
  how far the bug reaches, needs a PCI-ID table, and gets no testing on the
  hardware it claims to protect. The env override covers the same ground without
  pretending to know.
- **A config field.** Media entries have no `env` map in the schema, so the
  override is a session-level environment variable rather than something a
  parent sets per activity. That is the right shape for an escape hatch that
  exists to work around a driver bug.

## Reproducing it again

The scaffolding is worth rebuilding if this comes back on other hardware:

1. `sudo setfacl -m u:$USER:rw /dev/dri/renderD128` — otherwise everything below
   silently decodes in software and everything looks fine.
2. `./scripts/shepherd dev headless --gpu` — `--gpu` is what makes the session
   use the real GPU instead of pixman; without it `hwdec-current` is `no`.
3. Play a **cached** file (`~shepherd-kiosk/.cache/shepherd/media/videos/*.mkv`),
   not a synthetic clip. A `testsrc2` long-GOP clip did *not* reproduce it; real
   YouTube-sourced H.264 did, immediately.
4. Seek over IPC, screenshot with `grim`, and score greenness — `G − (R+B)/2`
   averaged over the video area, corrupt above ~20 and around −5 when clean.
   Eyeballing works too, but a number lets you run a matrix.

Confirm which path is live before trusting a result: `{"command":
["get_property","hwdec-current"]}` for mpv, or shepherd's own per-file log line.

# Opt-in resume: playback positions and the last-watched item

## Prompt

> Add a default-off option in shepherd-media (and also shepherd-media-android)
> that saves the playback position of each item in that library, as well as the
> last-viewed item. When reopening the library, it should offer to reopen the
> last shown item. With this option enabled, reopening any item should resume at
> the last position.

No issue number; this was a direct request.

## What was built

Off by default on both front-ends:

- **Linux:** a global `--resume` flag on `shepherd-media` (applies to `browse`
  and `play`).
- **Android:** a per-library `resume` field on `LibraryEntry`, surfaced as a
  "Resume playback" checkbox beside the existing "Reverse order" one.

With it on, per library: a position per item, plus the id of the item watched
most recently. Re-opening an item resumes it; re-opening the library shows a
"Continue watching" card offering the last item.

## Design decisions

**State model in `shepherd-media-app`, not in each binary.** The crate's README
opened with "on Linux, `shepherd-media` is stateless" — and this feature is the
exception that proves it. Putting `resume.rs` beside `settings.rs` is what lets
both front-ends share the policy (what counts as "finished", when a position is
too early to keep, how often to write), which is where the subtle behaviour
lives. The crate stays pure state + TOML; each binary decides *where* the file
goes (`$XDG_STATE_HOME/shepherd/media/resume/<library_id>.toml` on Linux,
`<filesDir>/resume/<id>.toml` on Android).

**State home, not cache home.** The Linux binary already had
`paths::media_cache_dir` for videos and posters, but a cache is defined by being
safe to delete and re-download. Positions are only re-earned by watching
everything again, so they get `paths::media_state_dir` (`$XDG_STATE_HOME`,
falling back to `~/.local/state`). Same reasoning on Android: beside
`settings.toml`, not under `cache/`.

**A start offset on the load, not a seek after it.** `PlayerHandle` gained
`set_start_position(Option<f64>)`, consumed by the next `play`, which the libmpv
backend turns into a `start=` per-file option on `loadfile` (alongside the
existing `audio-file=` for DASH audio). Seeking after playback begins would show
and decode the opening seconds first — visibly wrong on a resume, and wasteful
on a weak TV.

**`Session::set_start_position` is *not* consumed by one play.** Unlike the
`PlayerHandle` method it drives, the session re-applies it to every load of the
current item — including the automatic restart after a transient stream error.
Otherwise the existing retry logic (`RetryBudget`) would recover a dropped
stream by dumping the viewer back at the opening titles. The Linux UI keeps it
current with the live position while playing, and the Android app passes
`ResumeTracker::live_position()` on its own retry path.

**Ignore the player's position for the first 2 seconds of a playback**
(`SETTLE_AFTER_START`). A load carrying a start offset does not report that
offset instantly — for a moment mpv still answers with the previous file's
position, or zero. Recording that would erase the very position just resumed
from. This is why the per-frame position feed goes through `ResumeTracker`
rather than each UI calling `ResumeStore::record` directly: it is easy to get
wrong twice.

**Forgetting is a feature.** A stop within `NEAR_END_SECONDS` (30 s) of a known
duration, or within `MIN_RESUME_SECONDS` (20 s) of the start, *removes* the
entry. So an item watched to the end starts over next time, and the natural EOF
path needs no special casing — the last sampled position is near the end, and
the policy drops it. It also bounds the file: `retain_known` prunes items the
library no longer has when the state is loaded.

**Writes are batched** to at most one per `SAVE_INTERVAL` (10 s) while playing,
and forced when playback ends. The Linux UI also forces one when it sees
SIGTERM, before the player is torn down: shepherdd ending an activity mid-film
(a time limit, a bedtime window) is a *normal* way to stop watching here, not an
edge case.

**The card is shared UI.** `shepherd-media-ui::prompt` follows the existing
`grid` convention — custom-painted, focus by index, caller-supplied theme —
because both front-ends drive it from a D-pad or gamepad and neither can rely on
egui's focus traversal. It handles pointer and keyboard itself (an Android
remote's D-pad arrives as arrow keys and Enter, so that covers the TV for free);
gamepads stay the caller's job.

**The card is modal, but only while it is drawn.** Both UIs skip their grid's
input handling while it is up, or Enter would both activate a tile and press a
card button. On Linux there is a wrinkle: with `--connectivity-check` the grid
starts pessimistically empty of remote items, so the offered item may not be
listed for the first few seconds. The offer therefore *waits* up to
`OFFER_WINDOW` (10 s) for its item to appear and then lapses, rather than
blocking a grid the viewer is already using or popping up over one.

On Android, BACK needed the same treatment the source-dropdown popup already
had: it is snapshotted before the screen renders, so a BACK that dismissed the
card does not also leave the library behind it.

## Verification

Headless (`./scripts/shepherd dev headless`), with a two-item library of
ffmpeg-generated 3-minute clips:

1. Played `clip-a` for ~35 s, `SIGTERM` → `resume-demo.toml` contains
   `last_item = "clip-a"` and `position_seconds = 36.56`.
2. `play --item clip-a --resume` again → the transport overlay opens at `0:38`
   of `3:00` (the saved position plus the 2 s it took to screenshot).
3. `browse --resume` → the "Continue watching" card paints over the dimmed grid:
   "Test Clip A", "Left off at 0:50 of 3:00", Resume (focused) / Library.
4. Enter on the card → `STARTED_PLAYBACK item=clip-a`, overlay at exactly
   `0:50`. Escape on the card dismisses it and leaves the process running (a
   later Escape then exits the session, i.e. the grid got its input back).
5. Played `clip-b` for 25 s **without** `--resume`, `SIGTERM` → the state file
   is byte-identical. Default-off confirmed.

The Android front-end is verified by `cargo test`/`clippy` and the shared
`shepherd-media-app` tests only; it has not been exercised on hardware.

### Gotcha for the next agent: input in the headless session

`dev key` and `dev click` appear to do nothing to `shepherd-media` — it is an
egui/winit client, not GTK. `swaymsg -t get_seats` shows the headless seat with
**no input devices**, so winit never binds `wl_keyboard`. `wtype` creates a
virtual keyboard, sends its key, and exits immediately; the client loses the race
between the capability appearing and the key arriving.

The workaround is to keep the virtual keyboard alive across several presses, so
the later ones land:

```sh
wtype -s 700 -k Return -k Return    # instead of `dev key Return`
```

Pointer clicks (`swaymsg seat cursor`) did not reach it under any timing tried.

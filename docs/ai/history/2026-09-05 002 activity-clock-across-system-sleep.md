# Pausing the activity clock on system sleep — investigation (issue #155)

> Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/155>

## Prompt

> investigate #155

and, mid-investigation:

> there is a sleep both in the syslog and a session from `shepherd-kiosk` while
> an activity is running for you to check

and, on the findings:

> I meant the countdown hitting zero, not midnight, but yes we should be billing
> to the start day -- sessions crossing midnight are out of scope for now

and:

> I made #170 for that. Get started on #155.

## Issue text

> As observed, if the system sleeps while an activity is running, when it comes
> back, the time the device was asleep is counted as activity time. If it
> crosses 0:00, the activity never exits.
>
> Instead, we should pause the clock. (For some reason, I thought this was
> already built in by the use of monotonic time, but it seems not.) If the
> combined schedule means that the current wall clock time is now outside the
> activity's allowed hours, we should allow a global and per-activity
> configurable amount of time (default 2 minutes) for the child to save their
> progress if necessary, along with a visible warning.

## Headline finding

**The enforcement clock already pauses. The three countdowns the humans look at
do not.** `MonotonicInstant` wraps `std::time::Instant`, which on Linux is
`CLOCK_MONOTONIC` — frozen across suspend — so `deadline_mono` and
`billable_duration` genuinely exclude sleep. Every UI countdown, however, is
recomputed as `deadline - now()` from `ActiveSession::deadline`, the *wall-clock*
copy that is written once at launch and never corrected. So a sleep silently
pushes the real end of the session later while the displayed end stands still:
the child's HUD loses the sleep time, reaches 0:00, and then nothing happens for
as long as the device slept.

That is both halves of the report — "the time asleep is counted as activity
time" (in the display) and "if it crosses 0:00, the activity never exits" (the
display crosses zero; the engine has not). **Confirmed with the reporter: 0:00
is the countdown reaching zero, not local midnight.**

## Evidence from the journal

Session `23e1d56a-8826-4a3f-98e6-16ea4c984009` (`alice-wonderland`), boot `-2`,
2026-09-05, straddling two suspends:

| event | timestamp |
| --- | --- |
| `Session started … deadline=2026-09-05 13:01:28.016` | 12:01:28.032 |
| `Activity window appeared; billing starts here` | 12:01:29.555 |
| `PM: suspend entry (deep)` / `PM: suspend exit` | 12:01:36.286 → 12:01:40.941 |
| `PM: suspend entry (deep)` / `PM: suspend exit` | 12:01:47.519 → 12:01:52.935 |
| `Session ended … duration_secs=85` (`85.775640810s`) | 12:03:02.279 |

Wall-clock from window-ready to end is **92.72 s**; the engine billed
**85.78 s**. The 6.95 s difference falls inside the 10.07 s the machine spent in
the suspend path (kernel timekeeping suspends after the device freeze and
resumes before the thaw, so the excluded interval is always a little shorter
than entry→exit). `CLOCK_MONOTONIC` stopping across suspend is confirmed
directly, on this kernel, with a real session.

Corollary: the wall-clock `deadline` on that session (13:01:28) was, by the end
of those two suspends, ~7 s *earlier* than the monotonic deadline the engine
would actually enforce. The gap grows by exactly the length of every subsequent
sleep.

(The 16-hour suspend on 2026-08-31 07:40:26 → 23:54:46 in boot `-4` had
`has_session=false` throughout, so it shows the resume plumbing working but says
nothing about the session clock.)

## Where the two clocks live

- `crates/shepherd-util/src/time.rs` — `MonotonicInstant(std::time::Instant)`.
  Enforcement. Pauses on suspend. Correct.
- `crates/shepherd-core/src/session.rs` — `ActiveSession` carries **both**
  `deadline: Option<DateTime<Local>>` ("for display") and
  `deadline_mono: Option<MonotonicInstant>` ("for enforcement"), set together in
  `ActiveSession::new` and never reconciled afterwards.
- `CoreEngine::tick` (`engine.rs:1297`) checks warnings and expiry against
  `now_mono` only. `shepherdd`'s 100 ms tick timer (`main.rs:1161`) supplies it.

Every consumer of the countdown reads the wall-clock copy:

| consumer | site |
| --- | --- |
| HUD (`SessionStarted`) | `crates/shepherd-hud/src/state.rs:371` |
| HUD (`StateChanged` snapshot) | `crates/shepherd-hud/src/state.rs:487` |
| Launcher cover | `crates/shepherd-launcher-ui/src/state.rs:94`, `:187`, `client.rs:139` |
| Admin web dashboard | `shepherd-webui/src/pages/DashboardPage.tsx:173` (`useCountdown(deadline)`) |

`SessionInfo` already carries the *correct* monotonic-derived
`time_remaining` (`shepherd-api/src/types.rs:1024`, filled by
`to_session_info`) and it reaches the wire — **no client uses it.**

The HUD's own between-snapshot ticking is monotonic (`started_at: Instant` +
`time_limit_secs`, `app.rs:1049`), so it freezes cleanly during sleep; the jump
happens on resume, when `handle_event` recomputes the limit from the stale wall
deadline. `shepherdd` pushes a `StateChanged` on resume by design
(`main.rs:1222`), which is precisely what makes the drop visible.

## The second, larger hole: the schedule is never re-checked mid-session

`compute_max_duration` (`engine.rs:588`) clamps a session at launch to
`availability.remaining_in_window(&now)` — entry level and, via
`group_max_duration` (`:356`), group level. After that the window is never
consulted again. `tick` recomputes `evaluate_entry` for every entry, but only to
emit `AvailabilitySetChanged` for the launcher grid; the running session is
untouched.

So an N-second sleep moves the effective end of a session N seconds past the
window end that clamped it. A long sleep is unbounded: an activity started at
19:55 against a 20:00 bedtime, slept through overnight, resumes the next morning
with its remaining budget intact — inside a window that is closed, on a day
whose quota it was never checked against. Nothing in the engine ends it early.
This is what the issue's second paragraph is asking for, and it is the part with
actual supervision consequences.

## What the issue asks for, mapped onto the code

1. **Pause the clock.** Already true for enforcement. What is missing is
   *reconciling the wall-clock copy*: on `SystemResumed`, set
   `session.deadline = now + session.time_remaining(now_mono)` and broadcast.
   That fixes all four consumers at once with no wire change. (The alternative —
   move the clients onto `SessionInfo::time_remaining` — is cleaner but touches
   three UIs and does not help the `SessionStarted` event, which carries only a
   wall deadline.)

2. **Re-evaluate the combined schedule on resume.** The engine needs a
   resume hook it does not have today. `system_events.rs:205` broadcasts
   `SystemResumed` to clients and pings `resume_tx`; `main.rs:1222` handles that
   by re-broadcasting state. Adding an `engine.notify_resumed(now, now_mono)`
   call on that arm is the natural insertion point. It should ask the same
   question `evaluate_entry` asks — entry window, group window, daily quota,
   token balance — for the *running* entry, at the post-resume wall clock.

3. **Grace period + visible warning.** When that check fails, clamp the deadline
   to `now + grace` rather than ending immediately, and emit a Critical
   `EventPayload::WarningIssued` (the existing child-facing warning channel —
   `events.rs:64`; note the admin-facing channel from #143 is deliberately a
   different thing). `reduce_current` (`engine.rs:1700`) is the closest existing
   deadline-clamping path, but it clamps to ≥5 s and derives the new wall
   deadline from the old one rather than from `now`, so it would reintroduce the
   same drift — write a dedicated path instead.

4. **Config.** Follow the `cooldown_min_session_seconds` precedent exactly: a
   global `service.<name>_seconds` (default 120) resolved at load time into a
   per-entry/per-group `LimitsPolicy` field, with a
   `limits.<name>_seconds` override. See `policy.rs:731` (`LimitsPolicy`),
   `schema.rs:88` and `:718` for the raw pair, and the block at
   `config.example.toml:25` for how it is documented.

## Related defects noticed in passing (not part of #155)

- **Usage is billed to the wrong day across midnight.** `end_current_session`
  (`engine.rs:1469`) does `let today = now.date_naive()` and charges the whole
  session there. A session from 23:50 to 00:10 puts all 20 minutes on the second
  day; it should bill to the *start* day. Agreed as a real defect, but
  **explicitly out of scope for #155** — sessions crossing midnight are their
  own problem, and this one cannot produce "the activity never exits". Needs its
  own issue. Note that `add_usage` is not the only date-keyed call in that
  function: `settle_session_end` (`engine.rs:988`) passes the same `today` to
  `settle_tokens`, so token balances land on the same wrong day and whatever
  fixes this has to decide about them too. Cooldowns are safe — they are stored
  as `now + delta` timestamps, not keyed by date.
- **`reduce_current` drifts the same way.** `new_deadline` comes from the old
  wall deadline minus the reduction, not from `now + new_remaining`, so an admin
  reduction after a suspend leaves the two clocks disagreeing.

## Verification suggestion

The whole loop is reproducible without a graphical login: launch an activity
under the headless dev session (`./scripts/shepherd dev headless`), then
`systemctl suspend` with an RTC wake, and `dev shot` the HUD before and after.
A unit test at the engine level is cheaper for the schedule re-check — feed
`notify_resumed` a `now` past the window end and assert the clamped deadline and
the warning event — but the display half needs the real resume path, because it
is `StateChanged`-on-resume that triggers it.

## What was implemented

Branch `feat/155-pause-clock-on-sleep`.

### The resume hook

`CoreEngine::notify_resumed(now, now_mono)` (`engine.rs`), called from the
`resume_rx` arm of the main loop (`shepherdd/src/main.rs`), which already existed
for the suspend cover. It does two things:

1. `ActiveSession::resync_deadline` re-derives the wall-clock `deadline` from
   `deadline_mono`. This is the whole display fix: all four consumers read
   `deadline` out of the state snapshot, and `shepherdd` was already
   broadcasting one on resume, so correcting the session before that broadcast
   fixes the HUD, the launcher cover and the admin dashboard at once — no wire
   change, no client change.
2. `outside_allowed_hours` re-asks the *schedule* question (entry window, group
   window, force-enable override) at the post-resume wall clock. On a failure
   `ActiveSession::start_save_grace` clamps the session and a `Critical`
   `WarningIssued` goes out.

Ordering matters and is commented at both ends: the snapshot is broadcast
*before* the warning, because clients rebuild their countdown from the snapshot
and a warning delivered first would be overwritten by the state that followed.

### Decisions worth recording

- **Only the schedule is re-checked**, not all of `evaluate_entry`. A daily
  quota resets at midnight, so a long sleep can only leave *more* of it; a
  cooldown is not a reason to stop an activity that is already running. Adding
  either would end sessions for reasons the issue did not ask for.
- **The grace latches** (`ActiveSession::save_grace_started`). Suspend/resume is
  a loop a child can drive from the lid switch, and the monotonic clock does not
  burn the grace down while the machine is asleep — without the latch, closing
  the lid whenever the warning appeared would extend the session forever. With
  it, each wake returns only the grace that is left, so total awake grace is
  bounded by the configured value.
- **Warning thresholds at or beyond the grace are retired** when the clamp
  applies. Otherwise a default 300 s threshold fires on the very next tick and
  the HUD's last-writer-wins rendering replaces "you have 2 minutes to save"
  with a bare "only 120 seconds remaining", losing the explanation. Thresholds
  *inside* the grace still fire and still escalate.
- **`save_grace` cascades service -> group -> entry**, unlike every other limit,
  which is evaluated at both levels independently. One session has one answer;
  "strictest wins" is the wrong rule for a kindness. The entry's resolved value
  is used even when it was the group's window that closed — the child is saving
  the activity in front of them either way. Without the cascade,
  `save_grace_seconds` under `[groups.limits]` would have been a silently dead
  key, since `RawLimits` is shared.
- **`LimitDefaults`** replaced the two loose `Duration` parameters threaded
  through `Group::from_raw` / `Entry::from_raw` / `convert_limits`. Adding a
  third same-typed positional would have made a swapped pair mis-resolve every
  entry with nothing to catch it.

### Config

`service.save_grace_seconds` (default 120) and `limits.save_grace_seconds` on
entries and groups, following the `cooldown_min_session_seconds` precedent.
Documented in `config.example.toml`, both crate READMEs, and wired into the
config editor (`ServicePage`, `LimitsEditor`, `SubjectDetail`) so it is not a
key only reachable by hand-editing TOML.

### Tests

Eight engine tests covering the display correction, the clamp, the latch, the
force-enable bypass, a group window, a session already ending inside the grace,
`save_grace = 0`, and a resume with no session; two config tests for the
cascade and the default. `cargo test --workspace --all-targets` is green (62
suites) and `cargo clippy --workspace --all-targets -- -D warnings` is clean.

### Verified end to end, on hardware

Confirmed against a real suspend on `leibniz`, driven from the headless dev
session. Fixture (`dev-runtime/save-grace-fixture.toml`, gitignored): one entry
`bedtime-game` in a group `bedtime`, window 19:00-19:30, `max_run` 3600 s. The
entry sets **no** grace of its own and the service default is 120 s, so a
correct run has to resolve 30 s through the group — the cascade, exercised for
real rather than in a unit test.

Recipe:

```sh
./scripts/shepherd dev headless --no-build \
    --config dev-runtime/save-grace-fixture.toml --time "2026-04-27 19:28:40"
printf '{"request_id":1,"api_version":1,"method":"launch","params":{"id":"bedtime-game"}}\n' \
    | nc -U dev-runtime/shepherd.sock
sudo rtcwake -m no -s 150      # arm the wake alarm ONLY
sudo systemctl suspend         # ...and suspend through logind
```

**`rtcwake -m mem` does not work for this**, and cost a whole run to discover:
it writes `/sys/power/state` directly, so logind never emits `PrepareForSleep`
and `notify_resumed` never fires. That run is still worth recording, because it
reproduced the untreated bug on hardware: after a 151 s suspend the session was
still running at mock 19:31:25, **85 seconds past the 19:30 window end**, and
kept going until its monotonic budget ran out. Arm the alarm with `-m no` and
suspend with `systemctl suspend`.

The logind run, from the daemon log:

```
20:55:46.320  Session started         deadline=2026-04-27 19:30:44.765
20:58:28.076  Resumed; corrected the displayed deadline for time spent asleep
              slept_secs=146  deadline=Some(2026-04-27T19:33:11.142)
20:58:28.255  Resumed outside allowed hours; granting save-progress grace
              entry_id=bedtime-game  grace_secs=30
20:58:48.140  Warning issued          threshold_seconds=10  remaining_secs=9
20:58:58.121  Session expiring
20:58:58.257  Session ended           reason=Expired
```

Everything the design predicted, in order:

- `slept_secs=146` — the drift between the two clocks *is* the suspended time,
  so the wall deadline was 146 s stale. It was `19:30:44`, and the wall clock at
  resume was `~19:31:24`: **the HUD would have shown 0:00 with ~106 s of budget
  still to run.** Corrected to `19:33:11`.
- `grace_secs=30` — resolved through the group, not the service's 120 s.
- Only the **10 s** threshold fired. The defaults are 300/60/10 and both of the
  larger ones are `>= 30`, so they were retired by `start_save_grace` instead of
  firing at once and overwriting the explanation. The threshold *inside* the
  grace still escalated, as intended.
- Ended `reason=Expired` exactly 30 s after resume (`20:58:28` -> `20:58:58`).

The HUD 12 s into the grace showed `00:18` in critical red beside the banner
**"Time is up for now. You have 30s to save."**, over a mock clock reading
`7:31:37 PM` — i.e. visibly past the window it was launched under. Thirty
seconds later: "No session", and an empty grid, the entry being out of hours.

One cosmetic note, pre-existing and not touched here: the frame captured ~0.2 s
after resume still carries the suspend-cover placeholders (`--:--` clock, `--%`
battery) and the pre-suspend countdown, because the corrected snapshot has not
been painted yet. It resolves within the second, which is what the cover is for.

# The screen never comes back after the idle blank (#197)

## The prompt

> investigate #197. there is a log corresponding to this in
> `~/Downloads/journal--2026-09-11-1820.log`
>
> note that during the log, I couldn't get the screen to come back while in the
> session. I logged it out over the management API instead, and then the screen
> returned at GDM.

Issue #197, *"When the screen times out and turns off, there is no way to turn
it back on"* (no body). The log is from `copernicus`, a real device — not this
dev box.

## What the log shows

Three screen-power lines in 30 minutes, and never a wake:

```
17:51:10  Screen blank suppressed: an activity is running
17:53:26  Screen power set on=false
18:05:36  systemd-logind: Power key pressed short.  →  Suspending...
18:06:07  resume
18:06:54  a fresh gdm session (uid 60587)
18:09:44  Screen power set on=false
18:10:26  a fresh gdm session (uid 60588)
```

`shepherd_launcher: Screen power set on=true` never appears. The blank at
17:53:26 stayed for 730 s until the reporter pressed the power key — which
suspended the machine instead of waking the panel — and the blank at 18:09:44
stayed until the session was torn down. The screen came back at GDM because a
new session re-initialises the output, not because anything asked for it.

## Root cause

`sway.conf` binds `resume` to the wrong timeout.

```
exec swayidle -w \
    timeout 120 '$launcher --screen-off' \
    timeout 900 '$launcher --admin-idle-timeout' \
    resume '$launcher --screen-on'
```

In `swayidle(1)` an event is `timeout <secs> <cmd> [resume <cmd>]` — `resume`
is a *suffix of the preceding timeout*, not a standalone event. So the trailing
`resume` attaches to the 900 s administrator-idle timeout, and the 120 s blank
has no resume command at all. `swayidle -d` confirms the parse (note where
"Setup resume" lands):

```
Register idle timeout at 120000 ms
Command: echo off                    ← no "Setup resume"
Register idle timeout at 900000 ms
Command: echo admin
Setup resume
Command: echo on
```

`--screen-on` is the only thing in the tree that calls `set_screen_power(true)`
(`crates/shepherd-management/tests/supervision.rs:295` says as much: *"swayidle's
`resume` is the only thing that asks"*). So once the 120 s blank fires, the
panel can only come back if the seat also stays idle past 900 s — and that path
fires `--admin-idle-timeout`, which ends the session anyway. Touching the screen
under 900 s of idle does nothing. That is exactly what the log shows, twice.

## When it broke

Commit `61d490f` *"feat(admin): add administrator mode to the daemon and both
clients (#154)"* inserted the 900 s timeout **between** the 120 s timeout and
its `resume`, silently re-parenting the resume:

```diff
 exec swayidle -w \
     timeout 120 '$launcher --screen-off' \
+    timeout 900 '$launcher --admin-idle-timeout' \
     resume '$launcher --screen-on'
```

Before #154 there was one timeout, so the trailing `resume` was correct by
accident of ordering. Nothing checks a `swayidle` command's binding, and
`swayidle` reports no error for either form — the same class of silent failure
that #144 fixed on the `swaymsg` side.

## The fix

Reorder so `resume` sits next to the timeout it undoes:

```
exec swayidle -w \
    timeout 120 '$launcher --screen-off' resume '$launcher --screen-on' \
    timeout 900 '$launcher --admin-idle-timeout'
```

`$launcher --screen-on` is never suppressed, so it is safe on every resume.
The 900 s timeout needs no resume: leaving administrator mode is not something
to undo when the caregiver comes back.

## The guard

A comment alone would not have stopped #154 — the bug was appending to a line
that looked appendable. So `scripts/lib/headless.sh` now asserts the binding on
the derived sway config, alongside the existing `--no-harden-sway-ipc` /
`--no-restrict-ipc-peers` / `--no-state-custodian` exec-line assertions:

```sh
idle_line="$(sed -e :a -e '/\\$/N; s/\\\n//; ta' "$sway_config" \
    | grep -E "^exec swayidle " || true)"
...
if ! printf '%s\n' "$idle_line" | grep -qE -- "--screen-off' +resume +'[^']*--screen-on'"; then
    die "sway.conf's swayidle '--screen-off' timeout has lost its own ..."
fi
```

It joins the backslash continuations first, because the exec spans three lines
and a per-line grep would answer the wrong question. Checked against all three
shapes: the fixed config passes, the pre-fix ordering fires the `die`, and a
config with no `exec swayidle` at all fires the other one.

Every agent and human who boots `dev headless` runs this, which is the point —
the failure it catches is invisible until a device blanks in someone's hands.

## Verified end to end

In the headless session the fix reaches the real `swayidle`:

```
swayidle -w timeout 120 ./target/debug/shepherd-launcher --screen-off \
              resume ./target/debug/shepherd-launcher --screen-on \
         timeout 900 ./target/debug/shepherd-launcher --admin-idle-timeout
```

Then the behaviour, with no `dev shot` in the loop (it forces `dpms on` and
would have faked a pass): sit still until the blank, then inject real seat
input with `wtype` and watch `swaymsg -t get_outputs`.

```
18:31:07 start; dpms=True
18:32:02 BLANKED after ~55s idle (dpms=False)     ← idle had been accruing since boot
18:32:02 injecting seat input (wtype)
18:32:03 WOKE after ~1s (dpms=True) — PASS
```

**The A/B that proves it was the ordering**, not something else refusing to
wake. Two extra `swayidle` probes ran in the same session against the same
seat, one in each ordering, with `echo` commands and a 5-second blank:

```
probe BROKEN: timeout 5 <blank> timeout 3600 <admin> resume <wake>   # what #154 shipped
probe FIXED:  timeout 5 <blank> resume <wake> timeout 3600 <admin>   # this commit
```

Both blanked. Then, on the *single* input event at 18:32:02:

```
18:31:36 FIXED:  blank fired
18:31:36 BROKEN: blank fired
18:32:02 FIXED:  RESUME fired      ← woke
                                   ← BROKEN never resumed
18:32:07 BROKEN: blank fired       ← both simply re-blanked 5s later
18:32:07 FIXED:  blank fired
```

The broken ordering's resume never fires, because its 3600-second timeout never
fired and `swayidle` only resumes timeouts that idled. One input, two probes,
one wake.

## The durable lesson

`swayidle`'s `resume` binds to the timeout it *follows*. Adding a timeout to the
`swayidle` line is never a safe append — anything placed after an existing
`timeout … resume …` pair is fine, anything placed *before* the `resume` steals
it. Verify a change to that line with `swayidle -d` and read where "Setup
resume" lands, because neither `swayidle` nor sway will complain.

## Environment note

This dev box's own `swayidle` (pid 1796) is a stale install predating #154 —
`timeout 120 … resume …` with no 900 s timeout — so it does *not* reproduce the
bug. Reproducing it means the deployed `sway.conf`, not whatever is running
locally.

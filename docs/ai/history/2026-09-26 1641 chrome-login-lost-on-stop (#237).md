# A supervised Chrome forgot recent logins on stop (#237)

The prompt was "investigate #237. you may install flatpak and chrome globally in
this environment consistent with this project", followed by "yes, finish the fix
and commit it". The first version opted only a supervised Chrome into the
close-first stop; asked "wait what activity types do we close the window first
before signaling?" and told it was only readers and now Chrome, the answer was
"hm just do it for everything then". The issue:

> While setting up an activity, I noticed that it was losing its login between
> close/open. I suspect that this is because the profile is not being preserved
> between runs.
>
> ```toml
> [[entries]]
> group = "creative"
> id = "tinkercad"
> label = "Tinkercad"
> internet = { required = true }
> browser = { mode = "kiosk", profile_id = "browser" , start_url = "https://www.tinkercad.com" , wipe_on_exit = false }
> requires_input = ["keyboard", "mouse"]
>
> [entries.kind]
> app_id = "com.google.Chrome"
> args = []
> type = "flatpak"
> ```
>
> To test this, it may be easier to spin up a server that sends down a randomly
> generated/incrementing/similar cookie, then verify that it comes back after a
> close/open cycle.

and a follow-up comment:

> After further testing: it appears that this is only happening with the default
> profile (`profile_id = "browser"`) -- after changing it, the login is
> preserved.

## What was actually wrong

The profile directory was fine. Nothing deletes it, and `"browser"` (the web
UI's default `profile_id`) is not special to Chrome or to Lunchbox. What lost
the login was how the session ended.

A graceful stop of a flatpak activity goes through `kill_flatpak_cgroup`,
which ignores the signal it is handed and SIGKILLs every
`app-flatpak-com.google.Chrome-*` scope. Chrome keeps its cookie store in
memory and commits it to SQLite on a timer of about 30s, or at an orderly
shutdown. A kill inside that window drops whatever changed since the last
commit, and a login is exactly that.

The test was the one the issue suggested: a small Python server that hands out
an incrementing `sid` cookie and logs whether each request carried one. Against
com.google.Chrome 154.0.8037.57, with the cookie set 8s before the stop:

| stop                                        | cookie on next launch |
|---------------------------------------------|-----------------------|
| SIGKILL to the scope (what Lunchbox did)    | lost                  |
| SIGTERM to the scope                        | lost                  |
| SIGTERM to the main browser process only    | lost                  |
| close the window through sway               | kept                  |
| SIGKILL to the scope, but after 45s         | kept                  |

Through the real stack (`lunchbox dev headless` with the issue's entry pointed
at the server), profile `browser` stopped after 8s lost the cookie and profile
`tinkercad` stopped after 45s kept it. That is the pattern the follow-up
comment saw. The name made no difference; the second attempt had just been
left open longer before it was closed.

Note that switching the flatpak stop to SIGTERM would not have helped. Chrome
does not flush the cookie store on SIGTERM either, whether all of the scope's
processes get it at once or only the browser process.

## The fix

Issue #160 already added a polite close for readers: a graceful stop first asks
sway to close the activity's windows, waits up to `POLITE_CLOSE_TIMEOUT` (3s)
for it to exit, and only then falls through to the signal ladder. It was opt-in
per kind (`EntryKind::wants_polite_close`, true only for `Ebook`).

The first cut opted a supervised Chrome in beside readers. Once it was clear
that every other kind was one unmeasured app away from the same bug — Krita and
Prism Launcher are flatpaks that got an immediate SIGKILL on a "graceful" stop —
the opt-in went away: every graceful stop now asks first. The reasons it had
been opt-in did not hold up:

- **RetroArch** was left out because its single-SIGTERM shutdown was already
  verified to save (#125). Checked here with `libretro-gambatte` and a
  generated 32 KiB Game Boy ROM: on the close request RetroArch exits 0 in
  53 ms and writes `states/Gambatte/test.state.auto`, so the close is at least
  as good as the signal.
- **Steam** reads a close as "hide to tray". Its window attribution rarely finds
  a window in the launch process's group anyway, and when it does the cost is
  the 3s timeout before the unchanged signal path. Not measured here.
- **Flatpak and snap** "are signalled through their own cgroups" — true, but
  that is the signal, which comes after the close either way.

Measured in the headless session: Chrome closes itself in 150-300ms and the
cookie survives an 8s session under both profile ids; RetroArch as above.

`wipe_on_exit` is unaffected: the wipe runs from `finish_exit` when the process
monitor sees the activity gone, however it went.

## Left alone, worth knowing

- `kill_flatpak_cgroup` kills *every* scope for the app id, not only this
  session's, and ignores its `signal` argument.
- Current Chrome opens a first-run "Google Chrome and ChromeOS Additional Terms
  of Service" window on a fresh profile before loading `start_url`. In the dev
  session the management-setup overlay covers its Accept button, so the repro
  passed `--no-first-run` through the entry's `args`. Whether Lunchbox should
  add that flag itself is a separate question.
- To drive Chrome headlessly outside Lunchbox, launch it through the same shim
  (`flatpak run --command=bash com.google.Chrome -c 'exec /app/bin/chrome "$@"'
  bash …`). `--headless=new` skips the ToS window. For a headed window on the
  dev session's sway, add `--ozone-platform=wayland --no-first-run`.
- The example config's `play` group is outside its time window in the evening,
  and `terminal` is disabled; a test entry for the dev session belongs in
  `learn`.

# An admin UI for orphaned windows (web + companion)

Follow-up to `2026-08-21 002 activity-supervision-escapes.md`, which closed the
supervision escapes themselves but left one item open:

> **No admin UI for orphans.** `list_windows` / `act_on_window` exist over the
> wire but only as debug methods. The audit log now records escapes, which
> covers "was supervision lost?", but not "show me and let me close it".

That is what this adds. The transport was already there — the web UI has had a
"Sway Windows" page for as long as `list_windows` has existed, and #141 gave the
companion app the same panel — but neither could tell the child's game apart
from a surface no session owns, because **nothing on the wire said so**.

## The gap: the compositor does not know

`swaymsg -t get_tree` reports a pid per surface and nothing else. The mapping
from a pid to "this is the activity" or "this is nothing we know about" lives
entirely in `LinuxHost`, and until now it was used in exactly one place —
`report_unowned_windows`, which logs a warning and stops there.

So the fix is a wire field, not a UI trick. `WindowInfo` gains:

```rust
pub enum WindowOwner {
    Shepherd,   // our own furniture, and background processes we keep warm
    Activity,   // the session running right now
    Escaped,    // outlived its own teardown; the sweep is still killing it
    Unowned,    // nothing we know about
}
```

`crate::sway::list_windows` parses every window as `Unowned` — the compositor
genuinely cannot answer — and `LinuxHost::list_windows` attributes the list
before it leaves the host.

### Attribution has to match on more than the pid

`SupervisedPids` snapshots everything the host is accountable for, because the
window is very often *not* owned by the process we spawned:

- **process groups**, not pids: a launcher script that backgrounds the real
  program is reaped in milliseconds while its descendant keeps the surface.
  This is the same reason `spawn_window_watch` and `pgid_is_live` exist.
- **Steam game pids by app id**: a game is a child of the long-lived Steam
  client, so neither its pid nor its group is anything we spawned. Without
  this, every Steam window would read as `Unowned` *while the child is playing
  it* — the single worst false positive this feature could have.
- **sidecars** (touch bridge and friends), and the preloaded Steam client,
  which is a shepherd background process rather than an activity.

Escape is checked **before** ordinary supervision. The two overlap: a stop that
fails returns early via `?`, so the activity is still in `processes` *and* in
`escaped`, and "this got away from us" is the more urgent truth.
`an_escaped_activity_outranks_its_stale_process_entry` pins it.

The `/proc` walk for Steam pids runs only when some session actually is a Steam
session, so the common case costs nothing beyond taking the locks.

### Where the attribution runs, and where it does not

The obvious next move is to have the reconciliation sweep's
`report_unowned_windows` read `owner` too, so the log and the UIs share one
definition of "orphan". That was written and then backed out: resolving Steam
game pids walks `/proc` reading every process's environment, and the sweep runs
every two seconds regardless of whether anyone is looking. Paying that on a
kiosk to remove a latched, log-only false positive is a bad trade.

So the snapshot is built per `list_windows` call — on demand, from an admin
screen, at the 5s poll of whoever has it open — and the sweep keeps its own
cheaper `known`-set check. The two can therefore disagree at the edges: the
sweep will still warn once about a Steam game's window or a wrapper script's
descendant, neither of which is in `processes`. That is a wart worth fixing on
its own terms, not by making the hot loop expensive.

The UIs draw the sweep's other line by hand: an orphan stashed on the
scratchpad is hidden rather than loose on the child's screen, so it stays under
the scratchpad heading rather than in the Unsupervised section.

## The UIs

Both take the same shape, deliberately: a caregiver who learns one should not
have to learn the other.

- An **Unsupervised** section, rendered first, holding the on-screen windows
  whose owner is `Escaped` or `Unowned`.
- A **banner** above it naming the consequence rather than the mechanism —
  "time spent in these isn't counted and no time limit will end them" — and
  saying outright that the device will not close an unrecognised window by
  itself. The mechanism is in the audit log for whoever wants it.
- An **owner chip on every card**, not only on orphans. "Activity" next to the
  game the child is playing is what makes "Unowned" next to something else read
  as a fact rather than a guess.
- A one-line **explanation** on orphan cards only (`ownerDetail` returns null
  for the two healthy owners), and **Close promoted to the filled button** on
  them, since closing is what the section is listed for.

Scratchpad orphans stay under the scratchpad heading, matching the sweep's own
line.

`WindowPresentation` (companion) grew `isOrphan` / `ownerLabel` / `ownerDetail`
so the rules stay unit-testable off-device, and `WindowsUiState` grew
`orphaned` alongside `onScreen` / `scratchpad`.

## A bug the verification found

The web UI's window page **had never listed a single window**. `listWindows`
was typed `WindowsResponse` (`{windows: WindowInfo[]}`) and the page read
`data?.windows ?? []`, but `list_windows` has no `wrap_result` — the daemon
answers with a bare array. Every render fell through to "No windows reported".

It went unnoticed because the companion app decodes `List<WindowInfo>`
directly, so *its* panel worked; and because nobody had a reason to open the
web page and expect a specific window to be on it. Fixed by typing the helper
as `WindowInfo[]` and dropping `WindowsResponse`.

This is the argument for driving the real UI rather than trusting `tsc`: the
types were internally consistent and wrong.

## Verification

Unit and wire level:

- `window_owner_wire_spelling` (shepherd-api) pins the four JSON spellings both
  clients switch on, and `window owner decodes the unsupervised spellings`
  (Kotlin) pins the same strings from the other side. A rename now fails twice
  instead of silently downgrading every orphan to an ordinary row.
- `windows_are_attributed_to_what_is_supervising_them` covers all four owners
  plus the pid-less window; `an_escaped_activity_outranks_its_stale_process_entry`
  covers the overlap.
- Kotlin: `only escaped and unowned windows count as orphans`, `every owner has
  a chip label`, `only the owners that are a problem explain themselves`.

652 Rust tests and 43 companion tests pass; `cargo fmt --check`, `clippy -D
warnings` and `tsc --noEmit` are clean.

End-to-end, against `scripts/shepherd dev headless` with a fixture config, a
fixture activity that maps a real window, and a stray `foot` started outside
shepherd entirely:

```
shepherd   org.shepherd.launcher
activity   org.example.FixtureActivity
unowned    org.example.StrayWindow
```

- **Web UI**: Chrome driven over CDP (synthetic sway pointer clicks do not
  reach GTK widgets, but this is Chrome rendering its own surface, and the
  page's own click handlers work fine through `Runtime.evaluate`). The banner,
  the Unsupervised section, the chips and the promoted Close all render;
  pressing Close on the stray window removed it from the list.
- **Companion app**: installed on the USB-attached Pixel, already bonded to the
  dev session over BLE. Device controls → Windows… shows the red banner,
  "Unsupervised (1)", the `Unowned` chip in the error colour and the
  explanation; Close → "Close this window?" → Close window removed it, and the
  banner and section disappeared with it.

## Notes for the next agent

- `localhost:8080` on this dev box resolves to `::1` first, where
  `steamwebhelper` also listens — it answers 404 for `/api/v1/rpc` and looks
  exactly like a broken route. Use `127.0.0.1`.
- The management API's auth is open only when there is neither a static
  `auth_token` nor a claimed BLE admin. `<data_dir>/admin.toml` persists the
  claim, so pointing a fixture config at a fresh `data_dir` is the cheap way to
  get an unauthenticated HTTP API for a browser; the token is in that file
  otherwise.
- The web UI is embedded into `shepherdd` at compile time from
  `shepherd-webui/dist/`, so `npm run build` alone changes nothing that is
  running: touch `crates/shepherd-http/src/web_assets.rs` and rebuild
  `shepherdd` before booting a session to look at a UI change.
- `pkill -f <pattern>` in an agent shell matches the agent's **own** command
  line and kills the shell mid-command (exit 143/144). Bracket a character
  (`pkill -f "chrom[e]"`) or go through `swaymsg '[app_id="…"] kill'`.

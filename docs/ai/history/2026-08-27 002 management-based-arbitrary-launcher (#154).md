# Investigation: management-based arbitrary launcher (#154)

*2026-08-27* — investigation and scoping. Nothing implemented.

## The prompt

> investigate #154

followed, after the first pass, by four scope decisions from the project owner
(recorded under "Decisions taken" below).

## The issue

[#154 — Management-based arbitrary
launcher](https://git.armeafamily.com/albert/shepherd-launcher/issues/154),
opened 2026-08-28, no labels, no comments.

> One of the annoyances while setting up activities is the need to SSH in or
> temporarily switch to a non-shepherd DE (i.e. GNOME) to do things like:
> * log into Steam and install games
> * set up RetroArch controls
> * install packages of any kind
> * manage local files
>
> It would be convenient to be able to open anything that a non-shepherd DE
> would be able to directly from within shepherd via a general-purpose app
> picker that shows everything already registered via *.desktop files.
>
> To do this, a button in the management Web app and companion app could put
> shepherd in an "admin" mode that behaves a lot more like a regular DE. I'm
> envisioning a HUD that acts a lot more like a task bar:
> * At the very top left is effectively the Start button. When pushed, it opens
>   the list of known *.desktop files, enumerated much the same way GNOME
>   would. It should be searchable and fill the screen -- a reasonable
>   implementation could just be to reuse the existing grid but with a search
>   bar.
> * Instead of a single activity, windows are enumerated from the top left and
>   can be selected to switch between them.
> * The "X" button in the top right closes the currently focused window, and if
>   none are left, changes icon and is the "exit admin mode" button.

## The shape of the problem

The feature is a **mode**, not an activity kind, and that is what makes it
broad. Nearly every kiosk invariant in the codebase is written as an
unconditional truth rather than as policy, and admin mode has to make each one
conditional:

| Invariant | Where it is asserted | Admin mode | Resolution |
|---|---|---|---|
| One activity at a time | `shepherd-core::engine` / `ActiveSession` | many concurrent windows, no session | new daemon state beside the session machine |
| Nothing may go fullscreen but the launcher | `sway.conf` `for_window` | apps stay tiled | **accepted as-is** |
| Nothing floats | `sway.conf` `for_window [floating] floating disable` | dialogs stack full-size | **accepted as-is** |
| No window switching, no keybinds | `sway.conf` (no `$mod`), single workspace | switch between windows | sway binding mode + `WindowAction::Focus` |
| `Home`/`Ctrl+w`/`Alt+F4` end the session | `sway.conf` `bindsym --locked` | keys reach the app | sway binding mode |
| Screen blanks when no session runs | `swayidle` + `--is-idle-allowed` | stays awake | **suppress while in mode** |
| A window with no session is an orphan | `WindowOwner::Unowned`, orphan banner | not an orphan | **suppress banner while in mode** |
| Everything launchable comes from config | `launch(EntryId)` | launch by `.desktop` id | second spawn path |

## Decisions taken

Made by the project owner on the first pass of this investigation. Each one
removes a design branch; they are recorded here so the next agent does not
re-open them.

1. **Full-size stacked windows are acceptable.** `for_window [floating]
   floating disable` and `workspace_layout stacking` stay exactly as they are.
   Dialogs, file choosers and package-manager prompts become full-size stacked
   siblings rather than floating windows.

   *Consequence:* no imperative window-rule override, no `window::new`
   subscription, no fight with map-time `for_window` rules. This deletes the
   single largest piece of risk in the feature. It also makes the taskbar
   **load-bearing rather than cosmetic**: a modal dialog that has stacked over
   its parent is only reachable by switching to it, so some window switcher
   must exist before the mode is usable. Until the taskbar HUD lands, that
   switcher is the web/companion window list (see phasing).

2. **The orphan banner is suppressed while in the mode**, rather than adding a
   fifth `WindowOwner::Admin` and attributing admin-launched process groups.

3. **The idle timeout is suppressed while in the mode.**

4. **#144 is accepted as a known exposure for now**, and is a *release* gate
   rather than a merge gate: this work may land, but no release ships with
   admin mode until #144 is also addressed.

A second round, on the questions the first pass left open:

5. **The mode auto-exits after 15 minutes — but only when no windows are open.**
   If any window remains, the mode is held open and the HUD warns instead.
   Nothing is ever force-closed by the timeout.

   *Rationale, from the owner:* walking away is a legitimate workflow, not a
   failure to protect against — "waiting for Steam to download on a slow
   connection, etc." A timeout that force-closed windows would break the single
   most valuable thing the mode does.

   *Consequence:* the timeout is no longer the safety net for the walk-away
   case; decision 7 is. See "What the timeout does and does not protect".

6. **Admin mode does not consume the child's measured time.** No session is
   created, so no usage is billed. The audit log is the only record that the
   device was in use, which is what makes decision 9's audit variants
   load-bearing rather than nice-to-have.

7. **The "no windows left" gate is a HUD affordance only.** The HUD's "X"
   closes the focused window and becomes "exit admin mode" only once none are
   left, exactly as the issue describes. `exit_admin_mode` over HTTP and BLE is
   **never** gated: it always succeeds, closing whatever remains. So a window
   that refuses to close is rescued from the phone in seconds rather than
   stranding the device.

8. **The picker shows the whole `.desktop` set**, honouring `NoDisplay` and the
   rest of the visibility keys, with no shepherd-specific allowlist or category
   filter. The search bar is how you find things in it.

A third round, adding a lock:

9. **The HUD gains a lock button, and the device can only be unlocked from the
   companion or web app.** Locking is available locally; unlocking is not.

10. **When the idle timeout fires with windows still open, it locks instead of
    exiting.** This supersedes the "hold the mode open and warn" half of
    decision 5. The full rule is now:

    | Idle 15 min, windows open? | Result |
    |---|---|
    | No windows | exit admin mode → kiosk |
    | Windows open | **lock**; admin mode persists, nothing is closed |

    This is what decision 5 was reaching for and could not express: the Steam
    download keeps running, nothing is force-closed, and the child cannot touch
    the device. The "warn in the HUD" compensating control is no longer needed
    — the lock *is* the control.

11. **The lock is admin-mode-only, for now.** It was briefly scoped as a
    general device capability (lockable during an ordinary child session too);
    that was pulled back. Nothing in the mechanism becomes admin-specific as a
    result — only where the button appears and when it can be engaged — so
    generalizing later stays open. See "If the lock is ever generalized" for
    the one thing that would cost.

## What already exists and can be reused

More than the issue assumes, which is the good news.

**One RPC surface for all three transports.** `ManagementService`
(`crates/shepherd-management/src/service.rs`) is annotated with
`#[management_rpc]`; `dispatch_json` and the wire schema are generated from the
trait, and `shepherd-wire-codegen` emits the TypeScript (`shepherd-webui/src/api/
wire-types.generated.ts`) and Kotlin (`WireTypes.generated.kt`) mirrors. Adding
`enter_admin_mode` / `exit_admin_mode` / `list_desktop_apps` /
`launch_desktop_app` to the trait gets the IPC, HTTP and BLE transports and both
clients' types for free. This is the single biggest reason the feature is
tractable — the "button in the management Web app and companion app" is close to
free once the daemon side exists.

**Window enumeration and actions are already on the wire.**
`list_windows` → `Vec<WindowInfo>` (id, name, app_id, class, pid, workspace,
`in_scratchpad`, `visible`, `focused`, `owner`) and `act_on_window(id, action)`
with `Close` / `Hide` / `Show`. `WindowInfo::focused` is exactly what the
taskbar needs to highlight the current window, and `Close` is exactly the "X"
button. The web UI (`WindowsPage.tsx`) and the companion app
(`ui/windows/WindowsScreen.kt`) already render this list; both poll at 5 s.
See `docs/ai/history/2026-08-21 003 orphaned-windows-admin-ui.md`.

**`WindowAction` has no `Focus` variant.** That is the one missing primitive for
"windows … can be selected to switch between them". Adding it is a
one-line-per-layer change (`shepherd-api::types`, `sway.rs`'s command builder,
the wire-spelling tests in both languages).

**Both clients decide "orphan" in exactly one place.**
`WindowPresentation.isOrphan(w)` (Kotlin,
`ui/windows/WindowPresentation.kt:74`) and `isOrphan(w)` (TS,
`WindowsPage.tsx:94,228`). The section split, the owner chip colour and the
`ownerDetail` line all derive from that one predicate, so decision 2 is
"thread the mode into one function per client" rather than a UI rewrite. Both
have existing unit tests that extend naturally (`WindowPresentationTest`).

**`LinuxHost` already has the runtime-settable-flag pattern.**
`steam_launch_timeout_ms: Arc<AtomicU64>` and `steam_auto_dismiss:
Arc<Mutex<HashSet<_>>>` are set after construction by `configure_steam`
(`adapter.rs:407`), called from `shepherdd/src/main.rs:181`. An
`admin_mode: Arc<AtomicBool>` follows it exactly.

**The grid is already list-driven.** `LauncherGrid::set_entries(Vec<EntryView>)`
plus a `connect_launch(EntryId)` callback (`crates/shepherd-launcher-ui/src/
grid.rs`), with keyboard and gamepad selection already implemented. Feeding it
synthetic `EntryView`s built from `.desktop` files is the shape the issue
proposes and it fits. `EntryView.icon_ref` is a GTK icon-theme name or absolute
path, which is exactly what a `.desktop` `Icon=` key holds.

**Icon and XDG-path plumbing exists.** `crates/shepherd-config/src/icon.rs`
already walks `$XDG_DATA_HOME/applications` and `$XDG_DATA_DIRS/applications`
and reads `Icon=` out of the `[Desktop Entry]` group.

## What does not exist

**A `.desktop` enumerator.** `icon.rs` is a hand-rolled two-key scanner
(`Icon=`, `Exec=`), not a Desktop Entry implementation. A picker "enumerated
much the same way GNOME would" has to honour, at minimum: `Type=Application`,
`NoDisplay`, `Hidden`, `OnlyShowIn`/`NotShowIn` vs `XDG_CURRENT_DESKTOP`,
`TryExec`, localized `Name[..]`/`Comment[..]`, `Exec` field codes (`%f %F %u %U
%i %c %k`, and the deprecated `%d %n %v %m` which must be dropped), `Terminal=
true`, and id-based shadowing across the `XDG_DATA_DIRS` precedence chain
(`applications/foo.desktop` in an earlier dir hides a later one; subdirectories
produce `subdir-foo.desktop` ids). Desktop *actions* (`[Desktop Action …]`) can
be skipped for v1.

There is no such crate in the workspace and `Cargo.toml` pulls in nothing
freedesktop-shaped. Either vendor a small parser into `shepherd-config` (~300
lines with tests, no new dependency, and it can share `xdg_application_dirs()`
with `icon.rs`) or take a dependency. Given the project's habit of hand-rolling
narrow protocol code rather than importing a reactor's worth of transitive deps
(see #148 on `swayipc-async`), the vendored parser is the likelier call.

**A launch path that is not an `EntryId`.** `ManagementService::launch` takes an
`EntryId` and runs it through the whole policy engine — availability windows,
quotas, tokens, groups, cooldowns, firewall spec, input-compat sidecars. An
admin-mode launch has to bypass all of it. That is a *second* spawn path into
`HostAdapter::spawn`, and it must refuse to run unless the mode is active.

**A mode in the daemon.** There is no state between "idle" and "a session is
running". `ServiceStateSnapshot` has `policy_loaded`, `current_session`,
`entries`, `internet_status`, `diagnostics` — a client cannot ask "are we in
admin mode?". This wants an `admin_mode` field on the snapshot plus an
`EventPayload::AdminModeChanged`, so the HUD, the launcher and both management
clients switch presentation on the existing event stream rather than polling.
Every one of decisions 2 and 3 reads this field.

**A taskbar HUD.** `shepherd-hud/src/app.rs` is 1901 lines with a single
~900-line `build_hud_content`. A second, taskbar-shaped content builder is real
work, and #48 (HUD-free mode) wants a third arrangement of the same widgets.
Whoever does either should probably factor `build_hud_content` first.

**A virtual keyboard.** The search bar in the Start-menu grid assumes a
keyboard. On a tablet or TV — which is exactly where "I don't want to SSH in"
bites hardest — there is none. #6 (Virtual keyboard) is an unstated dependency
for the touch-only targets; the grid's existing gamepad/keyboard selection is
the fallback until then.

## The work, after the decisions

### The compositor: one rule, and it is a constraint not a mechanism

With decision 1, the only compositor changes admin mode needs are a **binding
mode switch** and a **focus command**. Both are ordinary sway commands and both
work today through `sway.rs`.

sway *binding modes* are runtime-switchable, unlike `for_window`. Wrapping the
kiosk grabs in a `mode "kiosk"` block and switching to a mode without them frees
`Home`, `Ctrl+w` and `Alt+F4`, and is the natural place to add Alt+Tab for admin
mode.

Worth noticing on its own: `bindsym --locked Home` and `bindsym --locked Ctrl+w`
grab those keys **globally today**, so no activity — a browser, a text field,
anything — has ever seen them. That is a standalone bug, fixable by the same
`mode "kiosk"` refactor, with no admin mode attached.

**The constraint:** every compositor command for this feature must be issued by
shepherdd through the host adapter, never by the launcher or HUD shelling out to
`swaymsg`. Under #148's `--harden-sway-ipc` the sway socket is unlinked after
shepherdd connects, so a UI client that shells out would work in dev and fail on
an installed device — the worst possible failure shape.

### Suppressing the orphan banner (decision 2)

Two sites, not one, and only the first is client-side:

* **The clients.** Thread the mode into `isOrphan` in both. The banner, the
  "Unsupervised" section, the promoted Close and the chip colour all fall out of
  it. Windows still list — that is how an admin closes one from the phone —
  they just stop being framed as a problem.
* **The daemon log.** `LinuxHost::report_unowned_windows` (`adapter.rs:811`)
  warns once per unrecognised pid, and admin mode would fill the log for its
  duration. Same false positive, different consumer. Suppress it from the same
  flag; it is nearly free once the flag exists.

The flag reaches `LinuxHost` as `admin_mode: Arc<AtomicBool>` following
`steam_launch_timeout_ms`. Note that `reconcile_escaped` and
`report_unowned_windows` are associated functions taking their state as explicit
`&Arc<…>` parameters rather than `&self`, so the flag is threaded in as one more
parameter. Because the management service holds `Arc<dyn HostAdapter>` and not
`LinuxHost`, the setter belongs on the **`HostAdapter` trait with a default
no-op**, alongside `ensure_shell_visible` / `list_windows` / `act_on_window`,
rather than as an inherent method like `configure_steam`.

### Suppressing the idle timeout (decision 3)

`sway.conf` runs `swayidle -w timeout 120 '$launcher --is-idle-allowed &&
swaymsg "output * dpms off"'`, and `--is-idle-allowed`
(`shepherd-launcher-ui/src/main.rs:217`) exits 0 exactly when
`state.current_session.is_none()`. It becomes
`current_session.is_some() || admin_mode` — one line, once the snapshot carries
the field.

### Security, given decision 4

Entering is properly gated: the trigger is a management RPC, so over HTTP and
BLE that means the bearer token or the BLE claim (`AdminAuthority`,
`admin.toml`). Being *in* it is the exposure, and decision 4 accepts the largest
part of that for now. What still needs deciding inside this feature:

* **The mode is exited deliberately, from the phone or the web app** — not by
  the timeout, per decisions 5 and 7. `exit_admin_mode` is ungated on those
  transports precisely so this always works.
* **A loud, persistent admin-mode indicator in the HUD is not optional.** With
  decision 5, a device can legitimately sit in admin mode for an hour with a
  Steam download running, and a child may well walk up to it in that state. The
  indicator and the audit log are the only things that make that legible after
  the fact.
* **Audit.** Admin-launched processes get no per-entry firewall
  (`SpawnOptions.firewall`) and no supervision — that is the point — so the
  audit log is the only record. `AuditEventType` needs variants for mode entry,
  mode exit, and each `.desktop` launched.
* **`launch_desktop_app` must refuse when the mode is inactive**, so it is not a
  second, permanently-open arbitrary-spawn RPC. (Under #144 an activity can call
  it regardless; that is exactly what #144 gates the release on.)

### Relationship to in-flight work

**#148 (open PR, not merged) is no longer a blocker.** The first pass of this
investigation concluded it was, because the imperative window-rule override
needed the `window::new` subscription it adds. Decision 1 removes that need, and
everything left (`mode`, `focus`, `list_windows`) works through `sway.rs` as it
stands. #148 remains a **rebase surface** — it rewrites `sway.rs` onto a held
IPC client — and the source of the hardening constraint above.

Also relevant:

* **#48 (HUD-free mode)** wants a second HUD arrangement; #154 wants a third.
  Shared refactor of `build_hud_content`.
* **#6 (Virtual keyboard)** gates the searchable picker on touch-only devices.
* **#106 (child-friendly file picker)** and **#105 (per-activity filesystems)**
  overlap with "manage local files"; #105's uid separation would close #144 as a
  side effect.
* **#75 / #2 (Android activities)** — Waydroid app launching has its own "list
  what's installed and launch one of them" picker. Worth checking for a shared
  shape before writing a second one.

## Suggested phasing

Each of these is independently shippable and useful.

1. **`WindowAction::Focus`** + the wire-spelling tests in Rust and Kotlin.
   Immediately useful to the existing web and companion window panels; no mode
   required. Half a day. **Promoted in importance by decision 1**: it is what
   makes phase 3 usable before any taskbar exists.
2. **Binding modes in `sway.conf`.** Wrap the kiosk grabs in `mode "kiosk"`;
   nothing switches out of it yet. Fixes the standalone bug that `Home` and
   `Ctrl+w` are globally grabbed from every activity. Small, and de-risks the
   mode switch.
3. **The daemon mode.** `enter_admin_mode` / `exit_admin_mode` on
   `ManagementService`; `admin_mode` on `ServiceStateSnapshot`;
   `AdminModeChanged` event; `HostAdapter::set_admin_mode` and the
   `report_unowned_windows` suppression; the `isOrphan` suppression in both
   clients; the `--is-idle-allowed` fix; the binding-mode switch; audit events
   for entry, exit and each launch; the 15-minute empty-case timeout
   (decision 5); ungated `exit_admin_mode` and buttons in the web and companion
   apps (decision 7). The timeout's **exit** branch only; its **lock** branch
   waits for phase 5. **No app picker and no taskbar yet** —
   admin mode at this stage is "the kiosk stops fighting you, and you drive
   windows from your phone", which alone covers "SSH in and start a thing".
4. **The `.desktop` enumerator** + `list_desktop_apps` / `launch_desktop_app`.
   Testable entirely off-device against fixture directories.
5. **The lock.** `shepherd-lock` on `ext-session-lock-v1`, `lock_device` /
   `unlock_device` (unlock never exposed as a local affordance), the lock
   button, and the timeout's lock branch. Admin-mode-only per decision 11, so
   no session-clock work is in scope.
6. **The taskbar HUD and the Start-menu grid.** Includes the mode indicator,
   the held-open-timeout warning, and the "X" that becomes "exit admin mode"
   only when no windows are left (decision 7 — the HUD is the *only* place that
   gate applies). The most visible work, and the most throwaway-able; do it
   last, when the semantics underneath are settled.

Release gate: **#144**, per decision 4.

## The lock

Decisions 9 and 10 add a genuinely new surface: nothing in this codebase locks
anything today. `grep` finds no `swaylock`, no session-lock protocol, no input
inhibitor. This is the largest single piece of new ground in the feature, and
it is also the piece with the clearest right answer.

### It has to be a real session lock, not an overlay

Two ways to build it:

**`ext-session-lock-v1`** — the Wayland protocol designed for exactly this. The
compositor guarantees the lock surface covers every output and receives all
input, and — the property that decides it — **if the lock client dies, the
session stays locked**. sway paints a solid colour rather than revealing what
is underneath.

**A `gtk4-layer-shell` overlay** on the overlay layer with exclusive keyboard
interactivity — the stack the HUD and `shepherd-pairing-display` already use,
and much faster to build.

The overlay is the wrong answer, and #148 already wrote down why in a different
context:

> an activity that issued `[app_id=org.shepherd.hud] kill` removed the HUD for
> the rest of the session

That same command against a layer-shell lock surface unlocks the device. A
session lock is immune: killing the client leaves sway showing a blank locked
screen, which fails closed. For a control whose entire job is keeping a
determined child out, fail-closed is the requirement.

### The dependency situation is better than it looks

`ext-session-lock-v1` ships in `wayland-protocols` (under `protocols/staging/`,
present in **0.32.12, already in this machine's cargo cache**), and
`smithay-client-toolkit` **0.20** — also cached — wraps it in a `session_lock`
module. So the protocol is reachable without adopting a new dependency
ecosystem.

The cost is that it is **not GTK**. Every shepherd UI today is GTK4
(`shepherd-hud`, `shepherd-launcher-ui`, `shepherd-pairing-display`), and a
session-lock client is a raw `wayland-client` surface that has to draw into its
own buffer. The lock screen's content is a message and maybe a window count, so
the drawing is trivial — but it is a second rendering stack in the workspace,
and that should be a deliberate choice rather than a surprise. (`gtk4-session-lock`
exists upstream as a sibling to `gtk4-layer-shell` and would avoid this; it is
**not** in the cache, so adopting it means a new dependency and a check that it
tracks the `gtk4-layer-shell` 0.4 generation this workspace pins.)

`shepherd-pairing-display` is the precedent for the shape: a small standalone
binary shepherdd spawns for one purpose. `shepherd-lock` follows it.

### The unlock must be graceful, and that is a real gotcha

Under `ext-session-lock-v1`, unlocking is a protocol message
(`unlock_and_destroy`). If shepherdd `SIGKILL`s the lock client instead, sway
keeps the session locked — **permanently, with no client left to unlock it**,
until the compositor restarts. So the unlock path cannot be "kill the process".

The clean version reuses machinery that already exists: the lock client
connects to shepherdd's IPC socket and subscribes to the event stream, exactly
as the HUD and launcher do, and calls `unlock_and_destroy` when it sees the
lock state clear. Killing it is then only the failure path, and the failure
path is "device stays locked", which is the safe direction.

### What the lock is worth, given #144

Stated precisely, because it is easy to overclaim in both directions.

`unlock_device` is a management RPC, so under #144 any process running as
shepherdd's uid can call it and unlock the device. That sounds fatal and mostly
is not: reaching that RPC requires running code, and the lock's threat model is
*a child at the keyboard of a locked device*, who by construction cannot start
anything. The realistic gap is a process that was **already running when the
lock engaged** — and in admin mode that means an application the admin
themselves left open, including, if they opened one, a terminal.

So: the lock holds against the case it is for, and #144 remains the thing that
makes it properly sound. This is one more reason decision 4's release gate is
the right call, and it means `unlock_device` should be among the first methods
gated when #144 is fixed.

### Smaller things the lock pins down

* **sway only runs `--locked` bindings while a session lock is up**, which the
  current `sway.conf` gets right by accident: `Mod4+Shift+Escape` (kill
  shepherdd, exit sway) is *not* `--locked`, so it cannot be used to escape a
  locked device, while the volume and brightness keys are and keep working.
  The three `--locked` bindings that call `--stop-current` (`Alt+F4`, `Ctrl+w`,
  `Home`) are no-ops in admin mode, since there is no session. Worth an
  explicit audit when phase 2 restructures the bindings, and worth a comment so
  nobody later adds a dangerous `--locked` binding without noticing.
* **A power cycle is the escape hatch.** Admin mode should not persist across a
  shepherdd or compositor restart, so rebooting a locked device returns it to
  the ordinary kiosk. That is a *safe* escape — the child escapes into the
  restricted environment, not out of it — and it covers the dead-phone case.
  This makes "admin mode is in-memory state, never persisted" a design
  requirement rather than an implementation detail.
* **The lock hides window contents by construction**, so the admin cannot watch
  a download progress bar through it. If that matters, the lock screen can show
  a count or a list of window titles from `list_windows`; it must not show the
  windows themselves.

### If the lock is ever generalized

Decision 11 keeps the lock inside admin mode, where there is no session and so
no quota to worry about. Should it ever become a general capability — lockable
during an ordinary child session — the requirement stated at the time was that
**a locked child session must not burn quota**, and that is the whole cost.
Recorded here because it was investigated and the answer is non-obvious:

* **Suspend is not a precedent.** `MonotonicInstant` wraps `std::time::Instant`
  (`CLOCK_MONOTONIC`), which does not advance across suspend, so sleeping the
  device pauses a session deadline *for free*. That is why nothing in the engine
  reacts to `SystemResumed` for accounting — `system_events.rs` only draws the
  suspend cover (#73). A lock is different: the machine is awake and the clock
  keeps running, so pausing has to be done deliberately.

* **The seam already exists.** `ActiveSession::billable_duration`
  (`session.rs:207`) is already distinct from `duration_so_far` — the split
  introduced for #135, so the child is not billed for the spinner before their
  window appears. Excluding locked time is the same idea one layer further:
  `ActiveSession` gains `locked_total: Duration` + `locked_since:
  Option<MonotonicInstant>`, and `billable_duration` subtracts them.

* **The deadline has to move too, and `extend_current` is the template.**
  Billing alone is not enough: a session locked for 20 minutes would otherwise
  expire 20 minutes of wall time early relative to the play the child actually
  got. `CoreEngine::extend_current` (`engine.rs:1650`) already shifts
  `deadline_mono` and `deadline` together *and* re-arms warnings via
  `warnings_issued.retain(...)`; an unlock does the same shift by the locked
  span.

* **Cleanest is to pause the tick, not to correct afterwards.** Otherwise
  warnings fire behind the lock surface — audio warnings to an empty room — and
  have to be un-issued on unlock. Skipping warning and expiry evaluation in
  `CoreEngine::tick` (`engine.rs:1295`) while locked makes the pause real rather
  than reconstructed.

Everything downstream (`settle_session_end`, cooldowns, the usage table) reads
the same `duration` value, so it follows automatically.

## Suggested phasing

Each of these is independently shippable and useful.

1. **`WindowAction::Focus`** + the wire-spelling tests in Rust and Kotlin.
   Immediately useful to the existing web and companion window panels; no mode
   required. Half a day. **Promoted in importance by decision 1**: it is what
   makes phase 3 usable before any taskbar exists.
2. **Binding modes in `sway.conf`.** Wrap the kiosk grabs in `mode "kiosk"`;
   nothing switches out of it yet. Fixes the standalone bug that `Home` and
   `Ctrl+w` are globally grabbed from every activity. Small, and de-risks the
   mode switch.
3. **The daemon mode.** `enter_admin_mode` / `exit_admin_mode` on
   `ManagementService`; `admin_mode` on `ServiceStateSnapshot`;
   `AdminModeChanged` event; `HostAdapter::set_admin_mode` and the
   `report_unowned_windows` suppression; the `isOrphan` suppression in both
   clients; the `--is-idle-allowed` fix; the binding-mode switch; audit events
   for entry, exit and each launch; the 15-minute empty-case timeout
   (decision 5); ungated `exit_admin_mode` and buttons in the web and companion
   apps (decision 7). The timeout's **exit** branch only; its **lock** branch
   waits for phase 5. **No app picker and no taskbar yet** —
   admin mode at this stage is "the kiosk stops fighting you, and you drive
   windows from your phone", which alone covers "SSH in and start a thing".
4. **The `.desktop` enumerator** + `list_desktop_apps` / `launch_desktop_app`.
   Testable entirely off-device against fixture directories.
5. **The lock.** `shepherd-lock` on `ext-session-lock-v1`, `lock_device` /
   `unlock_device` (unlock never exposed as a local affordance), the lock
   button, and the timeout's lock branch. Admin-mode-only per decision 11, so
   no session-clock work is in scope.
6. **The taskbar HUD and the Start-menu grid.** Includes the mode indicator,
   the held-open-timeout warning, and the "X" that becomes "exit admin mode"
   only when no windows are left (decision 7 — the HUD is the *only* place that
   gate applies). The most visible work, and the most throwaway-able; do it
   last, when the semantics underneath are settled.

Release gate: **#144**, per decision 4.

## The lock

Decisions 9 and 10 add a genuinely new surface: nothing in this codebase locks
anything today. `grep` finds no `swaylock`, no session-lock protocol, no input
inhibitor. This is the largest single piece of new ground in the feature, and
it is also the piece with the clearest right answer.

### It has to be a real session lock, not an overlay

Two ways to build it:

**`ext-session-lock-v1`** — the Wayland protocol designed for exactly this. The
compositor guarantees the lock surface covers every output and receives all
input, and — the property that decides it — **if the lock client dies, the
session stays locked**. sway paints a solid colour rather than revealing what
is underneath.

**A `gtk4-layer-shell` overlay** on the overlay layer with exclusive keyboard
interactivity — the stack the HUD and `shepherd-pairing-display` already use,
and much faster to build.

The overlay is the wrong answer, and #148 already wrote down why in a different
context:

> an activity that issued `[app_id=org.shepherd.hud] kill` removed the HUD for
> the rest of the session

That same command against a layer-shell lock surface unlocks the device. A
session lock is immune: killing the client leaves sway showing a blank locked
screen, which fails closed. For a control whose entire job is keeping a
determined child out, fail-closed is the requirement.

### The dependency situation is better than it looks

`ext-session-lock-v1` ships in `wayland-protocols` (under `protocols/staging/`,
present in **0.32.12, already in this machine's cargo cache**), and
`smithay-client-toolkit` **0.20** — also cached — wraps it in a `session_lock`
module. So the protocol is reachable without adopting a new dependency
ecosystem.

The cost is that it is **not GTK**. Every shepherd UI today is GTK4
(`shepherd-hud`, `shepherd-launcher-ui`, `shepherd-pairing-display`), and a
session-lock client is a raw `wayland-client` surface that has to draw into its
own buffer. The lock screen's content is a message and maybe a window count, so
the drawing is trivial — but it is a second rendering stack in the workspace,
and that should be a deliberate choice rather than a surprise. (`gtk4-session-lock`
exists upstream as a sibling to `gtk4-layer-shell` and would avoid this; it is
**not** in the cache, so adopting it means a new dependency and a check that it
tracks the `gtk4-layer-shell` 0.4 generation this workspace pins.)

`shepherd-pairing-display` is the precedent for the shape: a small standalone
binary shepherdd spawns for one purpose. `shepherd-lock` follows it.

### The unlock must be graceful, and that is a real gotcha

Under `ext-session-lock-v1`, unlocking is a protocol message
(`unlock_and_destroy`). If shepherdd `SIGKILL`s the lock client instead, sway
keeps the session locked — **permanently, with no client left to unlock it**,
until the compositor restarts. So the unlock path cannot be "kill the process".

The clean version reuses machinery that already exists: the lock client
connects to shepherdd's IPC socket and subscribes to the event stream, exactly
as the HUD and launcher do, and calls `unlock_and_destroy` when it sees the
lock state clear. Killing it is then only the failure path, and the failure
path is "device stays locked", which is the safe direction.

### What the lock is worth, given #144

Stated precisely, because it is easy to overclaim in both directions.

`unlock_device` is a management RPC, so under #144 any process running as
shepherdd's uid can call it and unlock the device. That sounds fatal and mostly
is not: reaching that RPC requires running code, and the lock's threat model is
*a child at the keyboard of a locked device*, who by construction cannot start
anything. The realistic gap is a process that was **already running when the
lock engaged** — and in admin mode that means an application the admin
themselves left open, including, if they opened one, a terminal.

So: the lock holds against the case it is for, and #144 remains the thing that
makes it properly sound. This is one more reason decision 4's release gate is
the right call, and it means `unlock_device` should be among the first methods
gated when #144 is fixed.

### Smaller things the lock pins down

* **sway only runs `--locked` bindings while a session lock is up**, which the
  current `sway.conf` gets right by accident: `Mod4+Shift+Escape` (kill
  shepherdd, exit sway) is *not* `--locked`, so it cannot be used to escape a
  locked device, while the volume and brightness keys are and keep working.
  The three `--locked` bindings that call `--stop-current` (`Alt+F4`, `Ctrl+w`,
  `Home`) are no-ops in admin mode, since there is no session. Worth an
  explicit audit when phase 2 restructures the bindings, and worth a comment so
  nobody later adds a dangerous `--locked` binding without noticing.
* **A power cycle is the escape hatch.** Admin mode should not persist across a
  shepherdd or compositor restart, so rebooting a locked device returns it to
  the ordinary kiosk. That is a *safe* escape — the child escapes into the
  restricted environment, not out of it — and it covers the dead-phone case.
  This makes "admin mode is in-memory state, never persisted" a design
  requirement rather than an implementation detail.
* **The lock hides window contents by construction**, so the admin cannot watch
  a download progress bar through it. If that matters, the lock screen can show
  a count or a list of window titles from `list_windows`; it must not show the
  windows themselves.

### One question this raises

**Should the lock be admin-mode-only, or a general device capability?**
*Recommendation: build it general, ship it wherever it is useful.* The
mechanism is identical, and a lock that works during an ordinary child session
is independently valuable — "dinner time" without ending the activity and
losing the child's progress. It also serves #48. Nothing about
`ext-session-lock-v1`, `shepherd-lock` or the RPCs is admin-specific; only the
button's placement is. Confirm, because a general lock interacts with session
time accounting (does a locked session still burn the child's quota?) in a way
the admin-mode-only version never has to answer.

## How to verify any of this

`scripts/shepherd dev headless` → `dev shot` / `dev tree` / `dev key` /
`dev click` → `dev stop`, per the `headless-dev` skill. The window panels in
both clients were verified this way for `2026-08-21 003`, including a stray
`foot` started outside shepherd entirely, which is precisely the fixture an
admin-mode window needs. Gotchas already recorded there and worth re-reading:
`127.0.0.1` not `localhost`; touch `crates/shepherd-http/src/web_assets.rs`
after `npm run build`; bracket a character in `pkill -f`.

# Administrator mode — rebase onto main, revalidate, and log out on exit (#154)

*2026-09-07.* The branch `feat/154-admin-mode` had been sitting since 2026-08-27
waiting on the two prerequisites the issue names. Both have landed.

## The prompt

> an outdated implementation of #154 is checked out. the prerequisites for
> implementing it securely should now be in place. refetch, rebase against the
> current main, and revalidate. also, make it so that "exiting" the admin mode
> actually logs out the session entirely -- actions in the admin mode may have
> created stale processes that make child processes behave differently than
> without them present

The issue's own comments say the same thing:

> This is going to have to wait until #161 and #156 for this to be done securely
>
> Also, "exiting" the admin mode should actually be a complete logout to ensure
> that everything is properly reset

Both are now on main: #161 (the state custodian, PR for #157) and #156 (web
management authentication, merged as PR #183). #144's IPC peer restriction —
the *release* gate decision 4 named — went in alongside them as #158.

## What the prerequisites bought

Worth stating precisely, because decision 4 recorded the exposure and this is
the entry that closes it.

* **#156 gates the trigger.** Every administrator RPC arrives through
  `POST /api/v1/rpc`, and `handlers::router` puts that route in the `guarded`
  half behind `require_auth`. There is no per-method work to do and no way to
  forget it: adding a route to `guarded` is automatically gated, and adding one
  to `open` is a visible act in a five-line list.
* **#157/#161 moves what the mode could damage.** Policy, the database and the
  audit log now live at `shepherd-state`'s uid, which nothing an administrator
  launches can read or write. Admin mode's audit records — the *only* trace it
  leaves, since it bills no usage — are behind that boundary too.
* **#144 draws a line the mode had to be taught to respect.** See below; this is
  the one the rebase turned up rather than resolved.

## The rebase

Seven commits onto `f729f5b`, sixty-odd merges later. Six files conflicted, all
resolvable, and `sway.rs`/`adapter.rs` were the interesting ones — main had
rewritten both around #144. Two things that a "rebased cleanly" report would
have hidden:

**Three files were committed with conflict markers still in them.** `git status`
shows an unmerged path as `UU`, but a file that was resolved by a script and
`git add`ed no longer appears there even if markers remain in the text. A
`git add -A` at the end of each conflict step therefore staged
`shepherd-ble/src/testsupport.rs`, `ManagementClient.kt` and
`RpcParams.generated.kt` with `<<<<<<<` in them. The compiler found the Rust
one; nothing would have found the Kotlin ones until an Android build. **Grep the
whole tree for markers before declaring a rebase finished** — `git status` is
not that check.

**The generated files needed regenerating per commit, not once at the end.**
Taking `--ours` on `WireTypes.generated.kt` and friends at each conflict gets
the rebase moving, but leaves every intermediate commit's checked-in mirror
disagreeing with its own Rust source, which `rpc_codegen_drift` fails on. The
fix that scales:

```sh
git rebase origin/main --exec \
  'cargo run -q -p shepherd-wire-codegen --bin rpc-codegen && git add -A && git commit --amend --no-edit'
```

Non-interactive, and it leaves each commit carrying the mirror its own sources
generate. `git rebase --exec 'cargo check --workspace --all-targets'` is the
companion check, and it is the one that caught four commits that did not build
on their own — see "One commit at a time" below.

## What main had moved under the branch

Four silent breakages. None was a conflict; each is a place where the branch's
change had been written against code that no longer exists.

* **The screen-awake rule had lost its home.** The branch taught
  `shepherd-launcher --is-idle-allowed` that administrator mode counts as
  "something is on screen". #144 deleted that flag — `swayidle` now calls
  `--screen-off` and the daemon decides under one lock, because a launch landing
  between the old gate's answer and the blank turned the screen off on a child
  mid-activity. The rule moved into `ManagementService::set_screen_power`,
  beside the session check it belongs with, and finally has a test.

* **`reconcile_escaped` went over clippy's argument limit.** #147 gave it
  `windows: Option<&[WindowInfo]>` (a failed compositor query is not an empty
  screen) and this branch added `admin_mode`, which makes eight. The three
  window-reporting parameters answer one question together, so they travel as
  one `OrphanSweep` now.

* **Two `getServiceState` helpers in `client.ts`.** #182 added one for the
  network page; the branch added one for the administrator page. Git merged both
  happily and `tsc` refused the result — which `npm run build` would not have,
  since rsbuild transpiles without checking types.

* **`shepherd-config` stopped building for wasm.** The `.desktop` enumerator
  calls `PermissionsExt::mode()`, which does not exist on
  `wasm32-unknown-unknown` — and `shepherd-config` is what the config editor's
  validator is compiled from. `cargo build --workspace` is green throughout;
  only `shepherd build config-wasm` sees it. The module is `#[cfg(unix)]` now.
  **A crate with a wasm consumer needs that build run, and the workspace build
  will not tell you.**

## The finding: administrator launches were inside #144's boundary

The revalidation's real result. `launch_unsupervised` — the spawn behind the
`.desktop` picker — left its child as a plain child of `shepherdd`, in
`shepherdd`'s own cgroup. That cgroup is exactly what #144's peer check trusts.

Every other launch path in the daemon respects that line, and
`shepherd-host-linux/README.md` states it as an invariant: *"nothing shepherd
starts for an activity is ever in shepherd's cgroup."* Firewalled entries get a
system-manager scope; snap and flatpak are scoped by their own runtimes;
everything else, Steam included, is wrapped in `systemd-run --user --scope`; even
`yt-dlp` is wrapped, because it parses remote input on a background timer.

The picker's launches had the weakest claim of any of them and were the only ones
inside. They are arbitrary third-party programs, chosen out of `.desktop` files
that an activity can itself write into `~/.local/share/applications` — where XDG
precedence puts them *ahead* of the system copies. A terminal opened in
administrator mode was a peer the daemon believed, holding `unlock_device`,
`launch` and the rest; and because these launches are deliberately `setsid`, it
kept them long after the mode ended.

So they get the same wrapper an activity gets (`admin_scope_argv_prefix`), and
the same fallback: run it bare when the user manager cannot be reached, which the
existing `ipc_socket_not_hardened` diagnostic already reports. Measured on the
headless stack, launching Calculator from the picker:

```
shepherdd 703633: /user.slice/…/tmux-spawn-….scope
calc      705036: /user.slice/…/user@1000.service/app.slice/
                  shepherd-admin-gnome-calculator-703633-0.scope
```

Two smaller things from the same lint pass, both branch code predating #144's
`helpers` module: the program name now resolves through `helpers::tokio_command`
(which narrows the picker to binaries in root-owned directories — deliberate: a
bare name a bare `$PATH` could satisfy is a name the kiosk uid can choose), and
so does `shepherd-lock`, whose bare-name fallback was a substitution primitive
aimed at the one binary standing between a child and a locked screen.

**The preloaded Steam client is `Unowned`, and that broke the HUD's exit.**
Found by driving it, not by any test. `snap run` re-execs, so the pid the host
recorded is not the pid that draws, and the compositor reports the stashed client
as belonging to nothing shepherd knows about. Main never noticed because
`report_unowned_windows` skips the scratchpad. `admin_windows` did not — so the
"X" never became "leave administrator mode" on any device that preloads Steam.
Which is every device that has it configured.

The first fix was to skip the scratchpad in `admin_windows` too, and it was
wrong in the other direction — which the next question asked of this branch
caught: *"what happens to the scratchpad rules when in this mode?"*

Nothing happens to them, and that is the point. sway binding modes swap
`bindsym` sets only; `for_window` is evaluated at map time and is global, which
`sway.conf`'s own admin block says. So the four
`for_window [class="^[Ss]team$"] move scratchpad` rules keep firing, and Steam
launched from the administrator picker is stashed the moment it maps. Measured:
five seconds after `launch_desktop_app("steam_steam.desktop")`, `list_windows`
reports `scratch=True visible=False`, and the HUD showed an empty taskbar and
the log-out icon — the "nothing is open" state — with Steam running, signed out
and unreachable from the device. Logging into Steam is the first thing issue
#154 asks for.

Both mistakes came from one predicate answering two questions, so it is two
predicates now:

| Question | Stashed windows? |
|---|---|
| `admin_windows` — what the taskbar lists | **counted.** The scratchpad is this compositor's "minimized", and a taskbar is how a minimized window comes back. Pressing the button works: `WindowAction::Focus` pulls a window off the scratchpad as well as raising it (verified against sway 1.11). |
| `admin_windows_on_screen` — what the "X" closes, and whether it offers the exit | **not counted.** Otherwise the always-stashed Steam preload holds the exit shut forever. |

Driven end to end after the split: Steam launched from the picker gets its
taskbar button while the "X" still offers the way out, and pulling it on screen
flips the button back to "close the focused window".

## Leaving the mode is a logout

The owner's second request, and the one that changes behaviour rather than
repairing it.

Turning the flag off was never a reset, and the mode's own design is why.
Everything a caregiver starts here is started deliberately outside supervision —
`launch_unsupervised` `setsid`s it so a package install survives a daemon
restart — so nothing in the daemon can enumerate what is still running, let alone
reap it. A signed-in Steam client, a dbus-activated service that was not there at
boot, a package manager still holding its lock: each changes how the *child's*
next activity behaves, and none of them appears in the window list the HUD's
"no windows left" gate reads. Ending the session is the only reset the daemon can
actually promise.

Both ways out now go through one `leave_admin_mode`, which asks for the same
shutdown `logout` has always asked for — so the ordering that makes `logout` work
(stop the session, drain HTTP, then tear sway down) carries the administrator
exit's own reply out ahead of the compositor going away.

Three decisions inside it:

* **Only when the engine really left the mode.** `exit_admin_mode` is idempotent
  and ungated on purpose — it is what rescues a device whose last window refuses
  to close — so the two clients and the timeout race each other. The one that
  arrives second must not tear down whatever session the device has moved on to.
* **The idle timeout's exit logs out too.** A mode nobody came back to leaves
  exactly the same processes behind as one somebody left on purpose.
* **The timeout's *lock* branch is untouched.** With work still on screen it
  still locks and keeps the mode, because walking away from a slow download is a
  supported way to use the mode (decisions 5 and 10). The timeout can therefore
  only ever end a session from the empty case, where there is nothing to lose.

Every button that leaves says so, since leaving closes whatever the caregiver has
open: "Turn off and log out" on the phone, "Turn Off & Log Out" in the web app,
"Leave administrator mode and log out" on the HUD.

### What the logout actually reaps, stated carefully

On a device, `host.logout()` ends the compositor, logind ends the session, and —
with no other session for that uid and lingering off — `user@<uid>.service` stops
with it, taking the `shepherd-admin-*.scope` units and everything in them. The
kiosk user is denied SSH and console login by `shepherd harden apply`, so "no
other session" is the normal case rather than a hope.

It is not the case on a *developer's* box, where an SSH session keeps the user
manager alive; a non-GUI process launched from the picker would survive there.
Measured in the headless session, every GUI client died anyway — a Wayland client
exits when its compositor does — but that is a property of the client, not a
guarantee from shepherd. The scopes are named, so stopping them explicitly on the
way out remains available if the session teardown ever turns out not to be
enough.

Also worth writing down: a device without getty auto-login comes back to a
greeter rather than to the kiosk. `shepherd harden apply` installs that override
inert, commented out (`/etc/systemd/system/getty@tty1.service.d/`
`shepherd-autologin.conf`), so on such a device the administrator exit needs
somebody to log back in. That is already true of the "Log Out" button both
management UIs have always had; it is now true of a second button.

## The lock was never installed

Reported from a device: `lock_device` failing with *"the binary didn't exist"*,
guessed to be fallout from the path hardening. It is not — it is simpler and
worse.

`shepherd-lock` is a workspace default-member, so `cargo build` produces it and
every development session locks perfectly. It was never added to
`SHEPHERD_BINARIES` in `scripts/lib/build.sh`, which is the **single** list that
`binaries_exist` checks after a build, `install_bins` copies to `$prefix/bin`,
`uninstall_bins` removes, and the `.deb` inherits (packaging drives `install.sh`
with `DESTDIR` set). So an installed device has `/usr/bin/shepherdd` and no
`/usr/bin/shepherd-lock`; `set_locked`'s sibling probe finds nothing, the
fallback resolves through `helpers` to `/usr/bin/shepherd-lock`, and the spawn
is `ENOENT`.

Reproduced exactly by moving `target/debug/shepherd-lock` aside against a live
headless session — that is the installed shape in miniature, `shepherdd` present
and its sibling absent:

```
could not lock the screen: Internal error: failed to start shepherd-lock:
No such file or directory (os error 2)
```

**The hardening is not implicated.** Before #144's `helpers` module the fallback
was `PathBuf::from("shepherd-lock")`, resolved through `$PATH` — and `/usr/bin`
is on `$PATH`, so it failed identically. The hardening changed which absolute
path is reported, not whether anything is there to report. Worth stating,
because "it broke after the hardening landed" is a plausible-sounding story that
would have sent the fix to the wrong place.

Two changes, and the second is what would have made the first unnecessary:

* `shepherd-lock` joins `SHEPHERD_BINARIES`. That installs it, packages it, and
  — verified by moving the binary and calling the function — makes
  `binaries_exist` *fail the build* when it is missing, rather than shipping
  without it.
* The error names the **resolved path**, not the name: "never installed" and
  "installed but unreadable" both answer `ENOENT`, and only the path says which
  directory was looked in. It now reads `failed to start the screen lock at
  /usr/bin/shepherd-lock: …`, which is the sentence that ends the investigation
  instead of starting one.

The resolution order and the packaging requirement are written into
`crates/shepherd-lock/README.md`, next to the code that depends on them.

The mechanism itself was never at fault: locking, the lock screen's own render,
and unlocking all work, verified in this session's headless run.

## End-to-end, on the headless stack

`dev headless` → `enter_admin_mode` → screenshot → `launch_desktop_app` →
`exit_admin_mode` → watch the session go.

* Administrator mode paints: taskbar with "Apps", lock button, and the launcher
  showing the searchable picker instead of the child's grid.
* `list_desktop_apps` returned 45 entries; `launch_desktop_app` on Calculator
  mapped `org.gnome.Calculator` and landed it in its own scope (above).
* `exit_admin_mode` replied `{"ok":null}` and sway was gone within a second, with
  no survivors — `shepherdd`, the HUD, the launcher and the Calculator all down.
* The audit log carries the whole span and nothing else does:
  `admin_mode_entered` → `admin_app_launched{Calculator}` →
  `admin_mode_exited{timed_out:false}`.
* Rebooting comes up as an ordinary kiosk: `admin_mode=false`, `locked=false`,
  no session, the child's grid.

The HUD's own exit button is verified as far as pixels go — with the scratchpad
fix the icon becomes the log-out glyph and the tooltip reads "Leave administrator
mode and log out". Pressing it is not driveable here: synthetic pointer events do
not fire GTK4 `connect_clicked` handlers in the headless seat. The handler takes
the same `admin_windows(..).is_empty()` branch the icon does, and calls the RPC
that was driven directly.

## One commit at a time

`git rebase --exec 'cargo check --workspace --all-targets'` failed on four of the
seven commits: the repairs above had been gathered into one commit *after* the
commits that needed them. They were split back into `fixup!`s against the commits
that introduced each problem, so `--exec` is now green for all ten. Worth the
round trip — the branch is a PR, and a bisect through a branch whose middle does
not build is worse than no bisect.

`GIT_SEQUENCE_EDITOR=: git rebase --autosquash -i <base>` applies fixups without
an editor, which is what makes this practical in a non-interactive shell.

## Gates

`cargo build --workspace --all-targets`, `cargo test --workspace --all-targets`
(69 suites, 0 failures), `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo fmt --all`, `shepherd build config-wasm`, `shepherd config validate`,
`npm run typecheck` / `test` (95) / `check:boundary` / `check:coverage`, and
`./gradlew :app:testDebugUnitTest`.

Two environment notes that cost time:

* **The box was out of disk** (94G full, 0 bytes free) and the first build failed
  as a bus error and "IO failure on output stream" rather than as a disk error.
  `target/debug` alone was 31G of mostly stale artifacts; removing it freed 31G.
  Check `df` before believing a linker crash.
* **Gradle needs JDK 21 and 26.04 defaults to 25.** The failure is a bare
  `* What went wrong: 25.0.4`, which names the version and nothing else.
  `CONTRIBUTING.md` says so; the error does not.

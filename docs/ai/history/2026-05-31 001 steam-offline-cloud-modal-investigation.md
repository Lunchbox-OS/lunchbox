# Steam activities do not open while offline (#50) — investigation

Forgejo issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/50>

## The problem (from the issue)

> Steam supports offline play but for some reason the activities are not
> loading when offline. All the user sees in this case is the loading
> spinner, forever.
>
> There's probably an account picker or some other dialog that needs to be
> acknowledged, but it is hidden by Sway.

And the reporter's GNOME repro (issue comment):

> 1. Disconnect the network
> 2. Open a regular Wayland GNOME session
> 3. Open Steam
> 4. Steam shows a "waiting for network" dialog for a few seconds before
>    giving up and opening
> 5. Attempt to open a game
>
> Steam then shows a warning message *as a modal within the main Steam
> window* regarding unsynced cloud saves. The modal needs to be confirmed
> before the game will open, but because it is part of the main Steam window,
> it does not appear when running within shepherd-launcher.

## Why CLI flags are a dead end (prior research)

Before instrumenting anything, we checked whether a Steam launch flag could
suppress the prompt. It cannot:

- `-offlinemode` ("always attempt to start in offline mode") only skips the
  connection-attempt delay; it does **not** suppress the cloud-sync warning.
- `-silent` (already used by shepherd's preload) is tray-only; no effect on
  the modal.
- `steam.cfg` knobs (`BootStrapperDisableSteamCloud`, `BootStrapperLogOnOffline`)
  and per-app `cloudenabled "0"` do not suppress it — Valve's own Steam Client
  Beta forum has multiple reports that the warning appears even with cloud
  globally disabled, plus an open `steam-for-linux` issue (#8784) where offline
  launches are broken outright. No `-skipclouddialog`/`-autoplayoffline` flag
  exists in either the Valve dev wiki or Bluscream's client-docs dump.

So the fix has to be on our side (compositor / detection), not a Steam flag.

## Experimental setup

Run on a libvirt VM (snapshot taken first), **GNOME / Wayland** session —
deliberately matching the issue's GNOME repro. Key environment facts:

- Steam is the **snap** package (`/snap/bin/steam`, `snap` rev 231).
- The Steam client UI is **XWayland + CEF/Chromium** (`steamwebhelper`).
- VVVVVV is installed, app id **70300**.
- cgroup v2 unified hierarchy.

### Launching the snap from a non-session shell

A plain `/snap/bin/steam` from the agent's shell fails with
`cannot find tracking cgroup` — the shell is not inside the systemd `--user`
session hierarchy snapd needs. `env -i` + `sudo -u …` makes it worse.

**Working invocation** (places Steam in a proper transient user scope):

```sh
systemd-run --user --scope --unit=steam /snap/bin/steam
```

Game launches forward to the running client the same way:

```sh
systemd-run --user --scope /snap/bin/steam steam://rungameid/70300
```

### Forcing Steam offline without killing the host (the firewall)

A blanket WAN-egress drop **also severs the agent's own model-API path** —
don't do that. The clean isolation is an **nftables rule scoped to Steam's
snap cgroup**. All Steam client processes (client *and* CEF `steamwebhelper`)
share a single cgroup:

```
/user.slice/user-1000.slice/user@1000.service/app.slice/snap.steam.steam-<uuid>.scope
```

Rule (allow loopback so the client↔CEF localhost IPC survives; drop the rest):

```
table inet steamblock {
    chain out {
        type filter hook output priority 0; policy accept;
        oifname "lo" accept
        ip6 daddr ::1 accept
        socket cgroupv2 level 5 "user.slice/.../snap.steam.steam-<uuid>.scope" counter drop
    }
}
```

- `DROP` (not `REJECT`) reproduces Steam's "waiting for network → give up"
  behaviour — it sees timeouts, like a yanked cable.
- Verified working by a climbing drop counter **and** Steam's own log:
  `ConnectionDisconnected('I/O Operation Failed')` →
  `Connectivity test: result=Failed, prev=Connected`.
- **Caveat:** the snap-scope `<uuid>` changes on every Steam restart, so the
  rule must be re-applied after a restart. A network namespace was tried first
  but fights snap confinement (joining a netns needs root, which breaks the
  user session snapd requires) — cgroup scoping is the right tool here.

This same mechanism (per-cgroup egress control) is what shepherd's own BPF
firewall already does, so it's representative of production.

## Key findings

### 1. The modal is *conditional*, not automatic

With clean save files, VVVVVV launches offline with **no modal**
(`cloud_log.txt`: `Failed sync for 'eval,' [login=false]` …
`Skipping un-modified file`). The blocking warning only appears when there is
a **pending local change that cannot be uploaded**. Reproduced reliably by
dirtying a save before launch:

```
(ValidateCache) File '…/qsave.vvv' SHA mismatch with cache - setting local changes
Failed sync for 'AC Launch,down,' [login=false][offlineMode=false]
```

When this fires, the game process never spawns until the modal is dismissed.

### 2. The exact modal (read from the live DOM via the CEF inspector)

- **Buttons:** `Play anyway | Cancel`
- **Text:** *"Unable to Sync Warning — Steam was unable to sync your VVVVVV
  saves with the Steam Cloud. If you play now, you may not have previous game
  progress and you may permanently lose it."*
- It renders **inside the main "Steam" window's DOM** (same browser context as
  the `STORE / LIBRARY / COMMUNITY` nav and the `NO CONNECTION` banner) — an
  in-window React modal, **not** a reliably separate top-level window.

### 3. Signal comparison — "is the modal showing right now?"

| Signal | Verdict |
|---|---|
| **CEF inspector** — create `<SteamRoot>/.cef-enable-remote-debugging`, restart Steam, then `http://localhost:8080/json` + DevTools `Runtime.evaluate` | **Definitive, and a control path.** Read the modal verbatim from the page target titled `Steam`; also **clicked "Play anyway" via `Runtime.evaluate` and the game launched**. Loopback-only, so the cgroup firewall doesn't block it. |
| **Compositor / X11 windows** | **Unreliable.** No consistent window: once a separate `"Launching…"` 600×286 toplevel appeared, once it was purely in-window. Any window it does spawn shares `WM_CLASS="steam"` with the main client, is `_NET_WM_WINDOW_TYPE_NORMAL`, and has **no `WM_TRANSIENT_FOR`** — only the title `"Launching…"` distinguishes it. |
| **AT-SPI / accessibility tree** | **Useless here.** Steam's CEF exposes only empty frames; the DOM is absent from the a11y tree unless launched with `--force-renderer-accessibility`. |
| **Steam logs** (`cloud_log.txt`, `connection_log.txt`) | **Good no-instrumentation heuristic.** `SHA mismatch … setting local changes` + `Failed sync [login=false]` + no game process within N seconds strongly implies the modal is up. |

### 4. Root cause on the shepherd (sway) side

This reframes the earlier scratchpad design discussion. The cloud-warning
surface — whether the separate `"Launching…"` window or the in-window modal —
carries `WM_CLASS="steam"`, so the existing kiosk rule
`for_window [class="^[Ss]team$"] move scratchpad` (`sway.conf`) hides it along
with the main client. The title-based rules (`^Steam$`, `^Steam - `) do **not**
catch `"Launching…"`. So the prompt exists but is swallowed by the same rule
that hides Steam — exactly the "hidden by Sway" symptom in the issue.

## Implications for the fix

- **Window-title matching is fragile** — the modal is often in-window, and when
  it is a window it's indistinguishable from the main client except by a
  generic `"Launching…"` title. The earlier "un-scratchpad on offline + re-hide
  on game window" sketch still works for the *separate-window* case but cannot
  catch the *in-window* case at all.
- **The robust handle is the CEF inspector.** shepherd could enable
  `.cef-enable-remote-debugging` on the Steam snap and, when an offline launch
  stalls, detect the `Unable to Sync` modal in the DOM and either surface it or
  auto-click **Play anyway**. Trade-off: it requires enabling Steam's debug
  port (loopback only), which is worth weighing as a follow-up.
- The detection can be backstopped by the cheap log heuristic (#3) so we don't
  hard-depend on the debug port.

## Proposed fix (sketch)

The findings change the recommendation. The modal is **usually an in-window
React modal** in the main Steam window's DOM, sometimes a separate
`"Launching…"` window, and in both cases the surface carries `WM_CLASS="steam"`.
So pure window manipulation can't reliably *find* it, and the only thing that
reliably *reads and dismisses* it is the **CEF DevTools endpoint**. The fix is
built around that, with window-surfacing kept as a degraded fallback.

### Decision: auto-resolve via CEF, don't surface

In a single-user kiosk the user's intent when launching offline is unambiguous
— *play the game*. Default behaviour: detect the `Unable to Sync` modal and
click **Play anyway** programmatically and invisibly, leaving the kiosk
aesthetic intact. Expose a policy toggle so the cautious choice (surface for
manual acknowledgement) is available.

### New components

1. **Enable the debug endpoint at preload.** `preload_steam()`
   (`crates/shepherd-host-linux/src/adapter.rs:137`) touches
   `<SteamRoot>/.cef-enable-remote-debugging` before `snap run steam -silent`,
   where `<SteamRoot> = ~/snap/steam/common/.local/share/Steam/`. The flag is
   read only at Steam start, so creating it in preload is the right seam.
   Rollout note: an already-running Steam needs one restart to pick it up.

2. **A `steam_cloud` module (host-linux) — a tiny DevTools client.**
   - `GET http://localhost:8080/json` → find the page target titled `Steam`.
   - Connect its `webSocketDebuggerUrl`, then `Runtime.evaluate`:
     - **Detect:** a button matching the affirmative action plus the
       "Unable to Sync"/cloud text.
     - **Resolve:** click **Play anyway** (proven in the investigation — the
       game then launches).
   - Pure loopback, so it coexists with any cgroup egress firewall as long as
     `lo` stays allowed.

3. **A launch watchdog tied to the `SteamSession` lifecycle.** This is the real
   structural addition. Today the monitor (`start_monitor`,
   `adapter.rs:188`) only flips `seen_game` when `find_steam_game_pids(app_id)`
   becomes non-empty and emits `Exited` when it later empties. **There is no
   timeout for "the game never appeared"** — precisely the forever-spinner in
   the issue. The watchdog fills that gap:

   ```text
   on SteamSession created:
     if host_offline:
         spawn watcher(app_id, deadline = now + ~30s):
             loop every ~750ms:
                 if find_steam_game_pids(app_id) non-empty: stop      # launched
                 if cef.detect_cloud_modal():
                     if policy.offline_autoresolve_cloud: cef.click_play_anyway()
                     else: surface_steam_window()                     # fallback
                 if now > deadline and still no game:
                     emit Exited{reason: LaunchStalled}               # graceful fail
   ```

   `host_offline` is an `Arc<AtomicBool>` on `LinuxHost`, set by
   `shepherdd/src/internet.rs`'s `InternetMonitor` (the `service.internet.check`
   target) — the same plumbing the window-surfacing idea also needs. The watcher
   dies when the game launches, the session ends, or the deadline passes.

4. **Config:** `steam.offline_autoresolve_cloud` (default `true`) and
   `steam.launch_timeout` (the watchdog deadline), validated in
   `shepherd-config`.

### Implications on the general Steam pipeline

The pipeline today is **preload → launch-by-app-id → poll for game PIDs →
scratchpad all Steam windows**. What each stage inherits:

- **Preload** gains one responsibility: ensure the debug flag exists before
  Steam starts. Note preload normally happens *online*, so Steam logs in and
  syncs cleanly — the modal therefore only appears when the user **played
  offline earlier** (creating un-uploaded local changes) and relaunches offline.
  The common path stays modal-free.
- **Launch** is unchanged on the spawn side, but the session now carries an
  implicit "is it allowed to stall?" contract. The watchdog converts an
  indefinite spinner into a bounded outcome: launched, or `LaunchStalled` → back
  to the launcher with a real error. That timeout is valuable on its own — Steam
  can stall for non-cloud reasons too.
- **The PID monitor** becomes the watcher's success signal:
  `find_steam_game_pids` going non-empty means both "game launched" (existing)
  and "modal resolved" (new). The watcher is a peer task to the existing 100 ms
  loop, keyed off the same `SteamSession`.
- **Window rules** (`sway.conf` scratchpad-by-class) are now understood as the
  *cause*, not the cure. With auto-resolve they stay as-is — we never show
  Steam. Only the fallback (`offline_autoresolve_cloud = false`) needs to
  surface a window, inheriting all the earlier "re-hide on game window / end
  session on Cancel" complexity. Auto-resolve sidesteps it — the main argument
  for making it the default.
- **Offline-state plumbing** is a new cross-crate dependency: internet status
  currently lives only in `shepherd-core`/`shepherdd` and is consumed at
  *gating* time; the host adapter has never needed it. Adding `host_offline` to
  `LinuxHost` (fed by `InternetMonitor`) is reusable for any future
  "we're offline" UI.
- **Firewall interaction (forward-looking).** `adapter.rs` currently bails with
  *"Firewall is not yet supported for Steam entries."* When that lands: (a) any
  cgroup egress rule on Steam **must keep `lo` open**, or it breaks both Steam's
  client↔CEF IPC *and* the DevTools channel; and (b) gating Steam's WAN via
  shepherd's own firewall will itself **trigger this exact modal** — so the
  cloud-modal watcher becomes a prerequisite for Steam network gating, not just
  for host-offline.
- **Security.** The debug port is full remote control of Steam's UI over
  loopback. In a locked kiosk with no untrusted local processes the exposure is
  low, but it is a real surface — document it, and ideally only enable it while
  the Steam pipeline is in use. Steam controls the bind, so we can't restrict it
  beyond loopback.
- **Brittleness / i18n.** DOM/text matching breaks if Steam restructures the
  dialog or runs non-English. Prefer matching the dialog's button *structure*
  (affirmative vs. cancel) over literal English, and gate the click behind the
  log heuristic (`SHA mismatch … setting local changes` + `Failed sync
  [login=false]` in `cloud_log.txt`) so we only act when a sync-failure modal is
  confidently the blocker.

### Fallback ladder

1. **CEF available** → detect + auto-click Play anyway (invisible, default).
2. **CEF unavailable / detection fails / `offline_autoresolve_cloud = false`** →
   surface the main Steam window for manual acknowledgement; re-hide when the
   game window/PID appears (the earlier sketch).
3. **Watchdog deadline hit** → `Exited{LaunchStalled}`: return to the launcher
   with an error instead of spinning forever.

Net change to the pipeline is small in surface area — a flag file at preload, an
`AtomicBool` of offline state, and one watcher task per offline Steam session —
but it converts the pipeline from "blocks indefinitely on an invisible modal" to
"resolves it, or fails cleanly with a reason."

## Reproduction recipe (condensed)

```sh
# 1. Launch Steam in a proper user scope (online), let it log in
systemd-run --user --scope --unit=steam /snap/bin/steam

# 2. Find Steam's snap cgroup, then block its egress (keep loopback)
P=$(pgrep -f '[u]buntu12_32/steam' | head -1); cat /proc/$P/cgroup   # → snap.steam.steam-<uuid>.scope
sudo nft -f steamblock.nft           # rule above, level 5 path = that scope

# 3. Dirty a save so there is a pending upload
printf '\nX' >> ~/snap/steam/common/.local/share/VVVVVV/saves/qsave.vvv

# 4. Launch the game → blocks on "Unable to Sync" modal
systemd-run --user --scope /snap/bin/steam steam://rungameid/70300

# 5a. (optional) read/dismiss the modal via CEF: touch .cef-enable-remote-debugging,
#     restart Steam, then DevTools Runtime.evaluate on the "Steam" page target.
# 5b. cleanup: sudo nft delete table inet steamblock; truncate the save back;
#     rm .cef-enable-remote-debugging
```

## Environment cleanup performed

- nftables `steamblock` table removed; host connectivity confirmed.
- The dirtied `qsave.vvv` was restored by truncating off the appended bytes
  (only appended, never overwrote) back to the original 2558 bytes, so it
  re-validates against the cloud cache rather than uploading a corrupt save.
- `.cef-enable-remote-debugging` flag removed; the transient `steamns` netns
  (from the abandoned namespace attempt) deleted. Steam reconnected (`Logged On
  … 'OK'`) once unblocked.

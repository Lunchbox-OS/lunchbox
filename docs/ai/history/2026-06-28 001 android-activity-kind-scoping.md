# 2026-06-28 — Scoping: Android activity kind (Waydroid)

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/2>

## Prompt

> Scope out #2.

Issue #2 — *Implement Android activity kind* — asks for an entry kind that
launches Android apps so the kiosk can offer activities like Microsoft 365
(Word/Excel), Khan Academy Kids, Duolingo, and Minecraft **Bedrock** Edition
(Java is already covered via Prism Launcher). The issue suggests **Waydroid**
on Linux and notes it may be worth **pre-booting** Android when any Android
activity is configured.

This document is a scoping pass only — no code changed. It maps what the
codebase needs, what Waydroid actually allows, where the two don't line up,
and a phased plan.

## Terminology warning: two unrelated "Androids"

The repo already says "Android" in two existing, **unrelated** contexts. Don't
conflate them with this issue:

1. **`[android-portability]`** flags in the media-launcher design docs, and
   `shepherd-host-android (Planned)` in `shepherd-host-api/README.md` — these
   are about porting **shepherd itself to run *on* Android** (a future host
   adapter). The `shepherd-media` Android port lives on a separate branch.
2. **Issue #2 (this doc)** — running Android **apps** *on Linux* via Waydroid,
   as one more entry kind alongside Steam/Flatpak. Shepherd stays a Linux host.

These share nothing in code. This work is the second one.

## TL;DR / recommendation

- The **config + plumbing** half is easy and well-trodden: add an
  `EntryKind::Android { package_name, .. }` and thread it through the same
  ~13 exhaustive match sites every kind touches. Steam is the closest model
  (external app manager + optional `[service.*]` config + background preload).
- The **runtime** half is where Android is genuinely new and where the real
  design work is. Two hard mismatches with the current architecture:
  1. **No host PID.** `waydroid app launch <pkg>` is fire-and-forget and
     returns immediately; the app runs as a process *inside* the LXC
     container. The entire current lifecycle (`HostEvent::Exited` driven by
     reaping a host PID in `adapter.rs`) does not apply. Exit detection and
     `stop()` must be rebuilt on **Wayland window lifecycle** + Android-side
     `am force-stop`, not signals.
  2. **One global container, shared by all apps.** Waydroid supports exactly
     one Android instance per machine — no per-app isolation, and therefore
     no per-entry network policy via the existing cgroup firewall, and no way
     to give two Android activities different sandboxes.
- There is also a **substantial non-code provisioning burden** (GAPPS + Play
  certification, x86→ARM translation for Bedrock, per-app sign-in, Device
  Owner kiosk lock-in) that is arguably bigger than the Rust change and is
  mostly host setup, not launcher code.
- Suggest landing this in **phases** (below), starting with a single
  hand-provisioned app (Khan Academy Kids is the lowest-friction) behind a
  capability flag, before promising the full app list.

## Part A — Waydroid realities that shape the design

Sourced from Waydroid official docs, the `android_hardware_waydroid` source,
and the Arch wiki; full citations in the research notes that produced this doc.
Items marked **(verify on hardware)** could not be pinned to an authoritative
number/string and must be confirmed on the real Ubuntu 25.10 target.

1. **Architecture.** A single privileged LXC container running LineageOS
   Android 13 on the host kernel. Two units: `waydroid-container.service`
   (root, container lifecycle) and `waydroid session` (per-user, bridges into
   the Wayland compositor). **One global instance — multi-instance is not
   supported** (upstream #566, unmerged PR #1990).

2. **Launch.** `waydroid app launch com.foo.bar` launches by package name and
   **returns immediately**. `waydroid app list` enumerates installed packages.
   Must wait for "Android with user 0 is ready" before launching or the app
   gets killed (#1066).

3. **Windows / Sway.** Default "full-UI" mode renders everything into one
   surface (`app_id="Waydroid"`). Setting
   `persist.waydroid.multi_windows=true` (then restarting the session) makes
   each app its own xdg-toplevel with **`app_id="waydroid.<package>"`**
   (confirmed in `wayland-hwc.cpp`). This is the mode we want: it lets Sway
   `for_window [app_id="^waydroid\..*"] fullscreen enable` and gives us a real
   window to watch. Caveats: Android surfaces keep their boot resolution and
   don't rescale on Sway resize (tune `persist.waydroid.width/height`);
   marking a *full-UI* window fullscreen can freeze rendering (#1611) —
   multi-window is the documented workaround. **(verify exact app_id on the
   installed base via `swaymsg -t get_tree`.)**

4. **Lifecycle / exit detection.** No process-exit event API. In multi-window
   mode, closing an app destroys its toplevel → the compositor sees a normal
   window-destroy (event-driven, no poll). Authoritative foreground/liveness
   is poll-only: `waydroid shell dumpsys activity activities | grep
   ResumedActivity`, `waydroid shell pidof <pkg>`. **No `waydroid app
   force-stop`** — kill via `waydroid shell am force-stop <pkg>`. Note window
   close ≠ process dead (Android may cache the process), so "session ended"
   should mean "we force-stopped it", not "the window closed".

5. **Pre-boot.** Pre-boot is the intended model and fits the issue's
   suggestion. Session boot is the one-time cost; app launches afterward are
   fast and the session survives window close. Keep warm cheaply with
   `persist.waydroid.suspend=true` (auto-freeze when idle) or
   `waydroid container freeze/unfreeze`. **Boot time and idle RAM are not
   authoritatively documented (verify on hardware)** — community ballparks
   (unverified) ~10–30 s cold boot, a few hundred MB–~1 GB idle.

6. **Installing apps + Google Play.** `waydroid app install file.apk`, or ADB.
   Play Store needs a GAPPS image (`sudo waydroid init -s GAPPS`) **plus**
   one-time device certification at google.com/android/uncertified. **x86
   gotcha:** a desktop is x86_64; many APKs (incl. Bedrock) carry ARM-only
   native libs and need a translation layer (libhoudini/libndk via the
   third-party `waydroid_script`). **Nvidia GPUs are unsupported by Waydroid**
   — relevant to Minecraft and to whatever GPU the kiosk has.

7. **Child lock-in.** Multi-window alone does **not** lock a child into one
   app — Android back/home/recents still work inside the window. The robust
   mechanism is **Lock Task Mode**, which needs a **Device Owner** DPC app
   provisioned over root ADB (`adb shell dpm set-device-owner ...`) plus
   `setLockTaskPackages`/`startLockTask`. There is no turnkey Waydroid kiosk
   toggle; we'd build/reuse a small DPC app. Defense in depth: replace the
   Android launcher, apply `DISALLOW_*` user restrictions, never allowlist
   `com.android.settings`.

8. **Network.** All Android traffic exits through the `waydroid0` bridge and
   the host FORWARD chain — a clean choke point, but a **global, per-machine**
   one. There is no per-app split (single container), and Waydroid does not
   persist hardened rules (#1250), so we own them and must re-apply on session
   start. **This means the existing per-entry firewall (issue #4), which keys
   off the activity's systemd scope/cgroup, cannot scope an individual Android
   app's traffic.** Android network policy is necessarily container-global.

9. **Host security (honest caveat).** Privileged LXC sharing the host kernel
   is a *usability* sandbox, not a hard boundary against hostile code; LXC
   upstream says privileged containers are not root-safe. Acceptable for a
   trusted-app child kiosk; not a containment story for untrusted code.

10. **Ubuntu 25.10 ("questing").** Install via the official `repo.waydro.id`
    (not base Ubuntu). Binder/binderfs is mainline since 5.14 and built into
    Ubuntu's generic kernel, so **no DKMS expected**. Wayland is mandatory
    (Sway satisfies it). **(verify: `questing` resolves in the repo, and
    `grep -iE 'binder|memfd' /boot/config-$(uname -r)`; fall back to `plucky`
    if the mirror lags — the package is distro-agnostic.)**

## Part B — The central design problem: lifecycle without a host PID

Every existing kind ends up as a host process (or systemd scope) that
`adapter.rs` tracks by PID and reaps, emitting `HostEvent::Exited`. Even the
browser activity — also windowed — is still a real Chrome host process. Android
is the **first kind with no host process to own**. So:

- **Spawn** is "ensure the session is up, then `waydroid app launch <pkg>`,
  then wait for the `waydroid.<pkg>` toplevel to appear" → emit
  `WindowReady`. There is no pid to put in the handle.
- **Exit detection** must come from watching that toplevel. Two viable
  designs:
  - **(B1) Window-watch:** a task subscribes to Sway events (or polls
    `list_windows()`, which already exists in `sway.rs`) for the
    `waydroid.<pkg>` app_id; when it disappears, force-stop the package and
    emit `HostEvent::Exited`. Cleanest, event-driven in multi-window mode.
  - **(B2) PID-proxy wrapper:** spawn a small host-side shim that runs
    `waydroid app launch` then blocks on `waydroid shell pidof <pkg>` polling
    and exits when the app dies. This synthesizes a host PID so the *existing*
    reaping path works almost unchanged — at the cost of a poll loop and a
    fake process. Lower-risk for a first cut; uglier long-term.
- **`stop()`** is `waydroid shell am force-stop <pkg>` (Graceful and Force can
  both map to it; Force can additionally `am kill`), **not** signals to a host
  pid. The codebase-map's suggestion to reuse the snap/flatpak cgroup-kill /
  `find_*_pids` + signal pattern does **not** apply here and should be
  replaced by the force-stop path.
- **Time enforcement / quotas / warnings** in `shepherd-core` are
  platform-agnostic and key off the session, so they work unchanged **once**
  spawn/exit are wired — that's the payoff for getting the handle right.

Recommendation: prototype with **(B2)** to get an end-to-end vertical slice
fast, then migrate to **(B1)** for a clean event-driven model. Either way the
session handle needs to carry the package name (and possibly the matched
con_id) rather than a meaningful pid.

## Part C — Config surface

Mirror the Steam/Flatpak shape. Per-entry kind:

```toml
[[entries]]
id = "khan-kids"
label = "Khan Academy Kids"
[entries.kind]
type = "android"
package_name = "org.khanacademy.android.kids"   # from `waydroid app list`
# args / env optional, forwarded to `waydroid app launch` / intent extras
```

Service-level (parallels `[service.steam]` → `RawSteamConfig`):

```toml
[service.waydroid]
preboot = true                 # start + keep the session warm at daemon start
suspend_when_idle = true       # persist.waydroid.suspend
boot_ready_timeout_seconds = 60
multi_window = true            # enforce persist.waydroid.multi_windows
```

`preboot` realizes the issue's "pre-boot Android if any Android activities are
configured" — but make it *config-driven* (default off, auto-on when an
`android` entry exists) rather than always paying the RAM cost.

Input note: Android apps are touch-first. On a touchscreen this is native; on
mouse-only hardware the existing `input_compat = "tablet_to_touch"` mode (and
`persist.waydroid.fake_touch`) is the relevant lever. Worth allowing the usual
`input_compat` stack on Android entries.

## Part D — File-by-file change map

Derived from a full sweep of every `EntryKind` match site. The enum/plumbing
list is mechanical (the compiler's exhaustiveness will enforce it — see the
recent "add missing browser field to Entry literal" commit); the runtime items
are the real work.

**Enums & plumbing (mechanical):**
- `crates/shepherd-config/src/schema.rs` (~L290) — add `RawEntryKind::Android`.
- `crates/shepherd-config/src/validation.rs` (~L119) — validate non-empty,
  well-formed `package_name` (Android package id charset).
- `crates/shepherd-config/src/policy.rs` (~L529, `convert_entry_kind`) — map
  raw → validated.
- `crates/shepherd-api/src/types.rs` — `EntryKind::Android` (~L94),
  `EntryKindTag::Android` (~L10), `EntryKind::tag()` arm (~L255).
- `crates/shepherd-host-api/src/capabilities.rs` (~L64, `linux_full`) — insert
  `EntryKindTag::Android` so the kind is advertised as supported.
- `crates/shepherd-config/src/bin/validate-config.rs` — summary string arm.
- `crates/shepherd-config/src/icon.rs` (~L24, `autodetect_icon`) — Android icon
  fallback (likely just the package name / a generic icon initially).
- `crates/shepherd-launcher-ui/src/tile.rs` (~L79) — fallback tile icon for
  `EntryKindTag::Android`.
- Test fixtures constructing `Entry`/`EntryKind` literals (engine.rs,
  http/tests, shepherdd/tests) — same churn the `browser` field caused.

**Runtime (the actual design work) — `crates/shepherd-host-linux/`:**
- `adapter.rs spawn()` (~L434) — Android arm: ensure session ready → launch →
  await `waydroid.<pkg>` toplevel → build a window/package-based handle (no
  meaningful pid). Add the package (and matched con_id) to `SessionInfo`.
- `adapter.rs stop()` (~L808) — Android arm: `am force-stop` for Graceful and
  Force; do **not** route through the signal/cgroup helpers.
- New exit-detection path (B1 window-watch or B2 pid-proxy) emitting
  `HostEvent::Exited` — this is net-new, not a tweak to an existing arm.
- `process.rs` — helpers, but `am force-stop`/`pidof`-based, **not**
  `find_*_pids` + signal as the analogy to Steam/Snap would suggest.
- `sway.rs` — reuse `list_windows()`; ensure `waydroid.<pkg>` matching +
  fullscreen. Possibly a `for_window` rule in `sway.conf`.
- New module (e.g. `waydroid.rs`) wrapping `waydroid session start/status`,
  readiness wait, prop set, `app launch`, `am force-stop`, freeze/suspend.
- `shepherdd` startup — optional preboot/keep-warm task, parallel to the Steam
  preload (`spawn_steam_preload`).

**Out of code (provisioning / ops — document, don't automate yet):**
- Host: `waydroid init -s GAPPS`, multi-window prop, network rules on
  `waydroid0`, optional ARM translation layer.
- Per device: Play certification; per-app sign-in; APK install.
- Kiosk lock-in: build/provision a Device Owner DPC app + Lock Task Mode.

## Part E — Per-app feasibility (from the issue's wishlist)

| App | Friction | Notes |
|-----|----------|-------|
| Khan Academy Kids | **Low** | Lightest; sideloadable, often works without full Google sign-in. Best first target. |
| Microsoft 365 | Medium | Has an x86 build; reliable sign-in/sync wants GMS (GAPPS). |
| Duolingo | Medium | Sideloadable but login often forces Google sign-in → GAPPS. |
| Minecraft Bedrock | **High** | Paid + Play-only license (needs GAPPS + certification + purchase), ARM-only libs → translation layer on x86, **Nvidia unsupported**. Most work, most caveats. |

## Part F — Open questions to resolve before/while building

1. **Verify on hardware:** exact `app_id` string; `questing` repo + kernel
   binder config; real boot time + idle RAM; per-app GMS behavior.
2. **Lifecycle choice:** B1 window-watch vs B2 pid-proxy for the first cut.
3. **Network policy:** accept that Android net policy is container-global
   (can't be per-entry like issue #4). Is a single global allowlist on
   `waydroid0` acceptable, or does that undermine differentiated activities?
4. **Lock-in scope:** is a child expected to be able to break out into the
   Android home screen? If "no", a Device Owner DPC app is in-scope and is its
   own mini-project. If the kiosk threat model trusts the child not to fiddle,
   multi-window + no settings allowlist may suffice for v1.
5. **GPU:** does the target have an Nvidia GPU? If so, Minecraft Bedrock is
   effectively out and software rendering may be needed.
6. **Provisioning ownership:** is APK/GAPPS/certification a manual runbook, or
   should shepherd automate any of it? Recommend manual runbook for v1.

## Part G — Suggested phasing

- **Phase 0 — Host spike (no launcher code).** Stand up Waydroid on the 25.10
  target, enable multi-window, install Khan Academy Kids, confirm the
  `waydroid.<pkg>` toplevel + Sway fullscreen + `am force-stop`, measure boot
  time/RAM. De-risks every "(verify on hardware)" above. **Do this first.**
- **Phase 1 — Config + plumbing.** Land `EntryKind::Android` and all
  mechanical match sites + `[service.waydroid]` schema, behind the capability
  flag, with no real spawn yet (or a stub). Pure, testable, low-risk.
- **Phase 2 — Runtime vertical slice.** Implement spawn/exit/stop for one app
  using B2 (pid-proxy) + preboot + readiness wait. End-to-end launch → play →
  exit → quota enforcement for Khan Kids.
- **Phase 3 — Harden.** Migrate to B1 window-watch; container-global firewall
  on `waydroid0`; input-compat; icon polish.
- **Phase 4 — Lock-in + more apps.** Device Owner DPC / Lock Task Mode;
  GAPPS-dependent apps (M365, Duolingo); Bedrock last (or defer if GPU/ARM
  blocks it).

## References

- Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/2>
- Existing model: `[service.steam]` / `RawSteamConfig`
  (`crates/shepherd-config/src/schema.rs`) + Steam preload in `adapter.rs`.
- Related: issue #4 (per-entry firewall) — note its cgroup model does **not**
  reach Android container traffic.
- Waydroid: docs.waydro.id; `waydroid/android_hardware_waydroid`
  (`wayland-hwc.cpp`, app_id); upstream issues #566, #1066, #1611, #1250.

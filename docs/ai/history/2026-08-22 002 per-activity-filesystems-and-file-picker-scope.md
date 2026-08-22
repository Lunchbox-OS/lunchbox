# Scope: per-activity filesystems (#105) and a child-friendly file picker (#106)

Date: 2026-08-22
Branch: scoped from `feat/media-kind` (no code changes)

## Prompt

> scope out #105 and #106

## The issues

**[#105 Per-activity filesystems](https://git.armeafamily.com/albert/shepherd-launcher/issues/105)**

> This will make it so that if the activity triggers the file picker, it is not
> possible to accidentally delete or overwrite files belonging to a different
> activity or even the system as a whole.

**[#106 More child-friendly file picker](https://git.armeafamily.com/albert/shepherd-launcher/issues/106)**

> This could supplement or even replace #105 for some use cases

Both are one-liners, so most of this document is establishing what the current
system actually does and what each issue would have to build.

## Where we are today

Every activity runs as **shepherdd's own uid**, in shepherdd's own `$HOME`,
with shepherdd's environment. `crates/shepherd-host-linux/src/process.rs:168`
(`INHERITED_ENV_VARS`) forwards `HOME`, `USER`, and all four `XDG_*_HOME`
variables straight through; `build_inherited_env` layers only the entry's
`[entries.kind.env]` on top. There is no per-entry data dir, no mount
namespace, and no uid separation anywhere in the spawn path
(`crates/shepherd-host-linux/src/adapter.rs:541` builds argv per kind and
hands it to the direct-spawn or firewall-helper path).

The per-activity **network** policy is the closest existing analogue, and its
shape is the template to follow:

| Layer | Network (exists) | Filesystem (#105) |
|---|---|---|
| Config | `[entries.firewall]` → `RawFirewallConfig` (`schema.rs:234`) | `[entries.filesystem]` |
| Validated | `policy.rs` | same |
| Wire | `SpawnOptions.firewall: Option<FirewallSpec>` (`traits.rs:78`) | `SpawnOptions.filesystem` |
| Enforcement | privileged helper via `pkexec` → `systemd-run --scope` + cgroup BPF | `bwrap` / `flatpak --filesystem` (no privilege needed) |

Sandboxing status per entry kind, as shipped:

* **Process** (tuxmath, ScummVM, `shepherd-media`, the example `terminal`
  entry) — no confinement at all. Full read/write over `$HOME` and anything
  the uid can reach.
* **Snap** (GCompris, Steam) — snapd's own confinement. The `home` interface
  gives the snap read/write to *all* non-hidden files in the real `$HOME`,
  shared across every snap that has it connected.
* **Flatpak** (Prism Launcher, Krita, Chrome) — the app's own manifest
  permissions, unmodified. `crates/shepherd-host-linux/src/adapter.rs:592`
  builds `flatpak run --env=… <app_id>` and passes no `--filesystem` /
  `--nofilesystem` flags, so Krita and Prism get whatever Flathub granted
  them, typically `--filesystem=home` or `host`.
* **Media / browser** — shepherd's own code; the browser already gets a
  per-profile `--user-data-dir` and a managed-policy JSON
  (`crates/shepherd-host-linux/src/browser.rs`).

### Why this is more than a tidiness problem

Three distinct failure modes, in increasing severity:

1. **Accidental clobber** — the case in the issue. Krita's save dialog opens
   on `$HOME`; a six-year-old saves `Untitled.kra` over
   `~/Games/Icons/Putt_Putt_Circus.png`, or deletes a ScummVM save.
2. **Cross-activity interference** — one activity's junk fills another's
   directories; there is no "this activity's stuff" boundary to reason about,
   which also makes "reset this activity" impossible to offer in the
   management UI.
3. **Policy integrity** — this one is not in the issue text and is worth
   raising. `service.data_dir` defaults to `~/.local/share/shepherdd`
   (`config.example.toml:10`) and holds both `shepherdd.db` (usage
   accounting, cooldowns, tokens) and `admin.toml`, which carries the
   management API's `http_token` and the BLE claim
   (`docs/ai/history/2026-07-20 001 groups-tokens-companion-validation.md:37`).
   Any activity that can browse to a file can read that token and call the
   management API to grant itself unlimited time, or delete the DB to wipe its
   daily quota. Today only social convention prevents it — the example config
   even ships a `terminal` entry (disabled, "For debugging only").

**But the filesystem is not the easiest door, and #105 cannot close the
easiest one.** `crates/shepherd-ipc/src/server.rs:128` assigns
`ClientRole::Admin` to every connection whose peer uid matches shepherdd's
own — which is every activity — and the dispatch path
(`crates/shepherdd/src/main.rs:1059`) never consults `info.role`; the role is
recorded in the audit log and otherwise unused. So `extend_current`,
`adjust_tokens`, `upsert_override`, `launch`, `reload_config`, and `logout`
are reachable over `$XDG_RUNTIME_DIR/shepherdd/shepherdd.sock` without reading
`admin.toml` at all, and `XDG_RUNTIME_DIR` is handed to every activity by
`INHERITED_ENV_VARS`. **This deserves its own issue**: a role check in
`dispatch_ipc` is almost certainly cheaper than the mount-namespace work, and
without it, hiding `admin.toml` protects the lock while the door stands open.

## #105 — Per-activity filesystems

### Feasibility is very different per kind

This is the crux of the scoping. There is no single mechanism that covers all
five kinds.

| Kind | Mechanism | Confidence | Notes |
|---|---|---|---|
| Process | `bwrap` mount namespace wrapping the argv | High | Verified on this box: `bwrap --ro-bind / / --tmpfs /tmp --proc /proc --dev /dev …` runs unprivileged even with `kernel.apparmor_restrict_unprivileged_userns=1`, because Ubuntu ships `/etc/apparmor.d/bwrap-userns-restrict`. Composes inside the existing scope: `pkexec helper → systemd-run --scope → bwrap → app`. |
| Media | same as Process | High | It is our own binary; we control what it needs (library file, cache dir, resume store). |
| Flatpak | `--nofilesystem=home --nofilesystem=host --filesystem=<per-entry dir>` on the existing `flatpak run` argv | High | Smallest possible change — a few lines in `adapter.rs:592`. Per-launch, no `flatpak override` state to leak between sessions. |
| Browser | Chrome managed policy (`DownloadDirectory`, `DownloadRestrictions`, `AllowFileSelectionDialogs`) + Flatpak filesystem flags | High | The policy writer already exists; it currently emits only `URLAllowlist`, `URLBlocklist`, `IncognitoModeAvailability`, `DeveloperToolsAvailability`, `ExtensionInstallBlocklist`. Adding keys is cheap. |
| Snap | **No good per-launch mechanism** | — | `snap run` builds its own mount namespace via snap-confine; we cannot inject one. The only levers are global and root-owned (`snap disconnect gcompris:home`), which is a *setup-doc* action, not a per-entry policy. |
| Steam | Same as Snap, plus games launch out of the Steam snap | — | Out of scope; document the limitation. |

Note the irony worth stating in the issue: the kinds that are hardest to
confine (Snap, Steam) are also the ones that already *have* confinement, and
the kind with none at all (Process) is the easiest to fix.

### Proposed config surface

```toml
[entries.filesystem]
# "shared"   — today's behavior (default until the flag day)
# "isolated" — private home, nothing else writable
mode = "isolated"

# Optional; defaults to <data_dir>/activities/<entry_id>
home = "~/.local/share/shepherdd/activities/krita"

# Extra roots, beyond the private home
[[entries.filesystem.paths]]
path = "~/Pictures/Shepherd"
access = "rw"          # "ro" | "rw"

[[entries.filesystem.paths]]
path = "~/Games/Data/Monkey"
access = "ro"
```

Design decisions worth settling before implementation:

* **Default mode.** `shared` keeps every existing config working; `isolated`
  is the safe default but is a breaking change for anyone whose ScummVM saves
  live in `~/.local/share/scummvm`. Suggested path: ship `shared` as the
  default with a validation *warning*, add a migration note, flip the default
  in a later release.
* **Migration.** Switching an existing entry to `isolated` should not look
  like data loss. Either seed the private home by binding the app's real
  config dir into it, or provide `scripts/shepherd fs migrate <entry>`.
* **A shared drop-box.** Isolation with no shared area means a drawing made in
  Krita can never be shown in a media activity. A single well-known
  `~/Pictures/Shepherd`-style root, `rw` for the few activities that opt in,
  covers the realistic handoffs without reopening `$HOME`.
* **Groups.** `[[groups]]` already carries schedule and limits; a group-level
  filesystem stanza (all "art" activities share one root) is a natural
  extension, but can wait.

### Phasing

* **P0 — Deny the crown jewels.** Ensure no activity can read or write
  `service.data_dir`, the loaded config file, the browser policy dir, or the
  IPC socket. For Flatpak this is `--nofilesystem`; for Process/Media it is a
  minimal bwrap profile (`--dev-bind / /` plus masks), which keeps GPU,
  Wayland, and D-Bus working by construction. See the limits below before
  treating this as a cheap standalone slice.
* **P1 — Flatpak + browser.** `--nofilesystem=home --filesystem=…` plus the
  three Chrome policy keys. Covers Krita, Prism, and the browser — the three
  activities most likely to open a picker today. Days, not weeks.
* **P2 — Process/Media via bwrap.** The real work. Building a bwrap profile
  that keeps Wayland, XWayland, PipeWire, D-Bus, `/dev/dri`, GPU drivers, and
  the snap paths working is where the schedule risk lives, and it interacts
  with the firewall helper's argv construction
  (`process.rs:110 firewall_helper_argv_prefix`) and with the input-compat
  sidecars, which need to see the same `XDG_RUNTIME_DIR`.
* **P3 — Docs + management UI.** Document the Snap/Steam limitation and the
  `snap disconnect …:home` recipe; add a "reset this activity's files" action
  now that "this activity's files" is a well-defined set.


### Limits of the P0 slice

Worth stating plainly, because P0 reads as an easy win and is not one:

* **It does not close the policy-integrity hole on its own.** The unauthenticated
  IPC socket above is the shorter path to the same powers. P0 must mask the
  socket too — and it lives beside the Wayland socket the activity needs, so
  that is a per-file mask, not a tmpfs over `$XDG_RUNTIME_DIR/shepherdd`. Even
  then, masking substitutes for a missing authorization check.
* **It is not independent of P2 for the Process kind.** Same uid means no
  permission bit helps; denying a path requires a mount namespace. The minimal
  profile is cheaper than full isolation, but the unprivileged-userns
  dependency and the `pkexec → systemd-run → bwrap` composition arrive with
  P0, not later.
* **It is a denylist, and the list grows silently.** It must be computed from
  resolved policy at spawn: `data_dir`, the config path (CLI/env-dependent),
  the `admin_file` and BLE factory-reset-marker overrides that may point
  outside `data_dir` (`policy.rs:443`), session logs, the media resume store
  (`~/.local/state/shepherd/media`, deliberately separate from the cache), and
  the Chrome policy dir and user-data-dirs under `~/.var/app/com.google.Chrome`.
  `write_policy_file` uses `std::fs::write`, which follows symlinks, and the
  URL allowlist depends on that file resolving inside the sandbox. Every new
  shepherd-owned path is a hole until someone remembers to add it; #105 proper
  inverts this into an allowlist.
* **Snap and Steam stay uncovered, and their current safety is incidental.**
  snapd's `home` interface grants non-hidden files only, and the default
  `data_dir` sits under `~/.local` — so GCompris cannot reach `admin.toml`
  today by accident of the default layout, not by design. A `data_dir` moved
  somewhere non-hidden loses that. Whether snap confinement also blocks the
  IPC socket is unverified. Wrapping `snap run` in bwrap also risks colliding
  with snap-confine's own namespace setup.
* **Adjacent protections are the distro's, not ours.** `kernel.yama.ptrace_scope`
  is 1 on the target, which blocks an activity from ptracing its parent
  shepherdd; that is worth an explicit startup check rather than an assumption.
  The BPF egress filter (`shepherd-firewall-bpf/src/main.rs:95`) matches on
  destination IP with no loopback exemption, so a `default = "deny"` entry is
  also cut off from the loopback HTTP API — but most entries carry no firewall
  stanza, and a Unix socket is not network traffic, so BPF never sees the real
  vector.
* **No user-visible benefit.** P0 does nothing for the accidental-clobber case
  the issue actually describes. It is hardening, not progress on #105.

### Risks

* bwrap profile churn per activity type — GPU and audio breakage shows up as
  "the game launches to a black screen", which is expensive to debug. Budget
  for iteration against real activities via the `headless-dev` skill.
* Unprivileged userns is enabled here today, but it is an Ubuntu AppArmor
  policy knob; a hardened deployment could turn it off. Detect at startup and
  degrade to `shared` with a loud log rather than failing to launch.
* Steam and Snap will remain unconfined by us. Say so in the docs rather than
  implying coverage.

## #106 — More child-friendly file picker

### What it means concretely

Implement a Shepherd-branded **`org.freedesktop.impl.portal.FileChooser`
backend** — a new crate (`shepherd-file-portal`) that xdg-desktop-portal
routes `OpenFile` / `SaveFile` / `SaveFiles` requests to, drawing a fullscreen
10-foot picker instead of GTK's tree view.

The pieces:

* A `.portal` file plus a `portals.conf` entry selecting our implementation
  for the Shepherd desktop (`XDG_CURRENT_DESKTOP=shepherd`). xdg-desktop-portal
  1.21, xdg-desktop-portal-gtk/wlr and `libportal` are all present on the
  26.04 target, so the runtime is already there.
* The UI itself. Two plausible hosts, and this is a real fork in the road:
  **GTK4**, matching `shepherd-launcher-ui`, or **egui**, matching
  `shepherd-media-ui` — whose poster grid (responsive tiles, explicit focus
  ring, drag-to-scroll, gamepad-friendly) is already exactly the interaction
  model a child-friendly picker wants, and is already shared between two
  front-ends. Reusing the media grid is the stronger option if the portal
  process can be an egui window; it means thumbnails, big touch targets, and
  gamepad focus come for free.
* Picker behavior: no path entry field, no hidden files, no navigating above
  the roots, roots supplied by the running activity's #105 policy, thumbnails
  for images/video, and confirm-before-overwrite phrased for a child.

### Coverage is the hard part

A portal only helps applications that *ask* the portal. Rough map for the
activities in `config.example.toml`:

| Activity | Picker today | Portal reachable? |
|---|---|---|
| Krita (Flatpak) | Qt dialog; portal when sandboxed | Yes — Flatpak apps use the portal by design |
| Chrome (Flatpak) | Chromium dialog; portal in sandbox | Yes (verify on the pinned Flathub build) |
| Prism Launcher (Flatpak) | Qt dialog | Yes |
| GTK apps run as Process | GTK dialog | Likely, via `GTK_USE_PORTAL=1` — **verify per GTK version** |
| Qt apps run as Process | Qt dialog | Likely, via `QT_QPA_PLATFORMTHEME=xdgdesktopportal` — **verify** |
| ScummVM | Its own in-engine browser | **No** |
| GCompris | Its own UI | **No** |
| Minecraft / mods | JVM-drawn dialogs | **No** |

So #106 covers the Flatpak and toolkit-dialog cases well and cannot cover
apps that draw their own browser. This is precisely why the issues are
complementary rather than alternatives: **#106 makes the common picker safe
and legible; #105 makes the pickers we cannot replace harmless.** The
Flatpak + portal pairing is also the one place where they actively cooperate —
a Flatpak launched with `--nofilesystem=home` can still save exactly where the
child chose, because the document portal binds the chosen file into the
sandbox. That combination is the design target.

### The keyboard problem

`SaveFile` needs a filename, and there is no on-screen keyboard
([#6 Virtual keyboard](https://git.armeafamily.com/albert/shepherd-launcher/issues/6)
is open). Options, in preference order: auto-name from context
(`Drawing 2026-08-22 3.kra`) with a rename affordance later; a word-picker
(adjective + noun tiles) as used for keyboard-free entry in
`docs/ai/history/2026-07-06 001 media-android keyboard-free library add.md`;
or block on #6. The auto-name default is almost certainly right for a child —
it also removes the "overwrite an existing file" path entirely for saves.

### Phasing

* **P1** — Portal skeleton: crate, D-Bus registration, `portals.conf`, and a
  hard-coded picker that returns one file. Proves the plumbing under the
  headless dev session with a Flatpak app.
* **P2** — Real picker UI (grid, thumbnails, gamepad/touch focus), roots from
  the activity's #105 policy.
* **P3** — `SaveFile` with auto-naming; overwrite confirmation.
* **P4** — Toolkit nudges for non-Flatpak activities (`GTK_USE_PORTAL`,
  `QT_QPA_PLATFORMTHEME`) added to `INHERITED_ENV_VARS`/per-kind env, once
  verified.

## Recommended ordering

1. **A role check in `dispatch_ipc`** — not part of either issue, and the
   single highest-value item found while scoping them. File separately.
2. **#105 P0** (deny `data_dir`/config/browser policy dir/IPC socket) — real
   hardening, no user-visible behavior change, but see "Limits of the P0
   slice": it drags in the bwrap dependency and is a denylist.
3. **#105 P1** (Flatpak + Chrome policy) — covers Krita/Prism/Chrome, which is
   where a picker actually appears today.
4. **#106 P1–P3** — the portal, with the per-entry roots #105 P1 defines.
5. **#105 P2** (bwrap for Process/Media) — the largest and riskiest chunk;
   worth doing after the portal exists so the confined activities still have a
   usable way to open and save files.

Doing #106 first in isolation is defensible — it is the visible win, and it is
what the issues are actually about. The hardening items above are worth
landing regardless, but they are a separate thread of work that scoping these
two issues happened to surface.

## Testing

* Unit: config parse/validate for `[entries.filesystem]`; bwrap and
  `flatpak run` argv construction (mirrors the existing
  `firewall_helper_argv_prefix` tests in `process.rs`).
* E2E: `crates/shepherd-e2e/tests/` already has `firewall_real_flatpak.rs` and
  `firewall_real_snap.rs` — a `filesystem_real_flatpak.rs` proving a confined
  Flatpak cannot write outside its root follows the same pattern.
* Visual: the `headless-dev` skill for the picker; the portal must be
  screenshot-verifiable over a running activity.
* Negative test worth having permanently: an activity attempting to read
  `admin.toml` fails.

## Open questions for Albert

1. Default `mode` — `shared` with a warning now and a flag day later, or
   `isolated` from the start with a migration script?
2. Is a single shared drop-box root (`~/Pictures/Shepherd`) wanted, or should
   activities be strictly disjoint?
3. Picker toolkit — GTK4 (matches the launcher) or egui (reuses
   `shepherd-media-ui`'s grid)?
4. Should the picker be a separate process/crate, or a mode of an existing
   binary? A separate D-Bus-activated process is the conventional portal shape
   and keeps a picker crash from taking the launcher down.
5. How much do Snap and Steam matter here? If GCompris's `$HOME` access is a
   real concern, the answer is a documented `snap disconnect` recipe, not code.
6. Do Android activities (#2 / #75) need their own answer, or does the Android
   runtime's own storage model cover it?

## Appendix: credentials, permissions, and ownership

Follow-up question while scoping: can plain permission / ownership changes
close both the `data_dir` hole and the IPC-socket hole, without the
mount-namespace machinery?

### The constraint that decides it

**Under a single uid, no permission or ownership change can separate shepherdd
from its activities.** They present identical credentials to every kernel
check, supplementary groups included. The `0060` trick (owner bits deny, group
bits allow) fails because the owner match is checked first and wins, and both
processes are the owner. Dropping a supplementary group for a child needs
`CAP_SETGID`; the user-namespace route is closed too, since an unprivileged
userns is created with `setgroups=deny` precisely to prevent that.

That leaves three families, only the first of which is really about
permissions:

* **A. Give activities a different uid** — permissions start working again.
* **B. Keep one uid and hide paths** (mount namespace) or **proxy the secret**
  (privileged helper). This is P0.
* **C. Keep one uid and authenticate the peer** rather than relying on the
  filesystem.

### A — uid separation

| Hole | Fix |
|---|---|
| Socket | `$XDG_RUNTIME_DIR/shepherdd/` mode 0700, kiosk-owned; the activity uid cannot traverse it. Worth a comment in `shepherd-ipc/src/server.rs` that the socket must stay a filesystem socket — an abstract socket ignores permissions entirely. |
| Data | `data_dir` 0700 kiosk-owned; per-activity homes owned by each activity uid, 0700. |
| Bonus | The per-activity home *is* #105. Cross-activity isolation falls out of ownership with no mount profile at all, and `get_peer_uid`'s `uid == getuid() → Admin` rule becomes meaningful instead of vacuous. |

Costs, which are real:

* **Privileged spawn.** The helper already execs `systemd-run --scope --uid=
  --gid=`, so the mechanism exists — but it deliberately enforces `--uid ==
  $PKEXEC_UID` (`shepherd-firewall-helper/src/main.rs:146`, and the README's
  trust boundary). Relaxing that to an allowlist (uids in a
  `shepherd-activities` group; never 0; never the kiosk uid) is a trust-boundary
  change and deserves its own polkit action id rather than riding
  `org.shepherd.firewall.apply-process`, which today covers every subcommand.
* **Privileged kill.** shepherdd cannot signal another uid's processes.
  `stop-scope` already exists, but `kill_by_command`, `kill_snap_cgroup`, and
  `kill_flatpak_cgroup` all assume same-uid signalling.
* **Session access.** `/run/user/1000` being 0700 is simultaneously the fix for
  the socket and the obstacle for everything else. Two workable routes:
  `setfacl -m u:shepherd-act:--x /run/user/1000` plus entries for `wayland-1`
  and `pipewire-0`, leaving `shepherdd/` without an entry — POSIX ACLs are
  verified working on that tmpfs — or pass a connected `WAYLAND_SOCKET` fd at
  spawn and skip the ACL for Wayland. XWayland also needs a readable
  `XAUTHORITY` copy, and snap/flatpak/Steam each need a real home plus their
  own `/run/user/<uid>` (`loginctl enable-linger` per activity user).
* **Unverified:** the session bus policy for a cross-uid connection (matters
  for #106's portal, which would see a uid other than the session owner), and
  snap/flatpak behavior under a non-login uid.

### C — peer authentication, available today

Ownership cannot help the socket under one uid, so authenticate provenance:

* Gate `dispatch_ipc` on `info.role`, which is currently computed and only
  logged. Split the trait into observer methods (state, entries, volume,
  brightness) and mutating ones (`adjust_tokens`, `upsert_override`,
  `extend_current`, `reload_config`, `logout`); nothing a child-facing client
  needs falls in the second group.
* Decide the role by *who the peer is*, not what uid it has. shepherdd spawns
  the launcher and the HUD itself, so keep their pidfds and grant Admin only on
  an exact match. `SO_PEERPIDFD` (Linux 6.5+; the target runs 7.0) gives a
  race-free peer identity; `nix` does not wrap it, so it is a raw
  `getsockopt(fd, SOL_SOCKET, 77)`. A cgroup-provenance variant ("is the peer
  inside the session scope?") is weakened by the supervision escapes in
  #135–#137; the spawned-pidfd allowlist is not.
* Both clients reconnect per request
  (`shepherd-launcher-ui/src/client.rs:174` onward), which is why passing a
  pre-connected fd at spawn is awkward and the pidfd allowlist fits better.

For the data half under one uid, the credential can leave the uid's filesystem:
`admin.toml` owned by `root:root` 0600, read and rotated through a new helper
subcommand gated like the firewall action. The SQLite DB is too chatty to proxy
and stays a mount-mask or accepted-risk item.

### Recommendation

Do the role gate plus peer provenance now — code-only, no new users, groups, or
polkit changes, and it closes the hole P0 could not. Then, if #105 is done
properly, **do it as uid separation rather than mount namespaces**: it closes
both holes with plain 0700, yields cross-activity isolation as a side effect,
and replaces "did we remember to mask this path?" with "is this file owned by
the kiosk user?". The bwrap work becomes optional polish rather than the
foundation.

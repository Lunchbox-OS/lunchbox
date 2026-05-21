# Web browser activity type

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/10>
Depends on: <https://git.armeafamily.com/albert/shepherd-launcher/issues/4> (Firewall rules)

## Goal

Add a supervised web-browser activity type to shepherd-launcher, primarily to
support school workflows (Google Workspace, Microsoft 365) and DRM-protected
media. Per #10, Chrome (proprietary) is the target browser because Chromium
and Firefox have gaps for those use cases on Linux.

The activity must:
- run sandboxed
- respect a configurable URL allowlist
- respect a configurable network firewall (#4)
- support per-entry browser profiles, with optional sharing across entries

## Non-goals

These are inherited from the project's stated non-goals
(see [README.md](../../../README.md#non-goals)) and must not be violated:

1. No patching of Chrome itself.
2. No circumventing DRM or platform protections.
3. No telemetry or PII collection by shepherd-launcher.

The browser activity type wraps Chrome via supported, documented mechanisms
only (Flatpak, Chromium enterprise policy JSON, systemd transient scope
properties).

## Architecture

The browser activity is a **composition** of three concerns, two of which
are reusable for other activity types. There is no new `EntryKind` variant.

### 1. Sandbox: existing `EntryKind::Flatpak` + `com.google.Chrome`

Use the official `com.google.Chrome` Flatpak from Flathub. It is the
supported sandboxed Chrome distribution on Linux:

- bubblewrap isolation comes for free
- Widevine works (DRM/protected media)
- Flatpak handles updates
- it slots into the existing per-entry cgroup-scope kill path in
  [crates/shepherd-host-linux/src/process.rs](../../../crates/shepherd-host-linux/src/process.rs#L101-L162)
  (`kill_flatpak_cgroup`)

Decision: Flatpak Chrome is the only supported browser path. `firejail` and
native `.deb` Chrome are explicitly rejected for this iteration (more moving
parts, less consistent sandboxing model).

### 2. Browser policy (new, reusable for any Chromium-based browser)

A new `[entries.browser]` config section. The spawn path materializes it
into:

- a per-entry user-data-dir under
  `~/.var/app/com.google.Chrome/config/google-chrome/<profile_id>/`
- a Chromium [enterprise policy JSON][chromium-policies] under
  `~/.var/app/com.google.Chrome/config/chromium/policies/managed/<entry_id>.json`
  (regenerated on each spawn from the entry config)

[chromium-policies]: https://chromeenterprise.google/policies/

Hostname-level allowlisting is enforced by Chrome itself via `URLAllowlist`
/ `URLBlocklist` policies (no extensions required). Lockdown lives here too:
disable dev tools, disable incognito, disable extension installation,
restrict download targets, force kiosk/app mode, etc.

Schema sketch:

```toml
[entries.browser]
profile_id = "school"        # filesystem segment under user-data-dir
mode = "kiosk"               # "kiosk" | "app" | "windowed"
start_url = "https://classroom.google.com"
url_allowlist = [
    "https://*.google.com/*",
    "https://*.googleusercontent.com/*",
    "https://accounts.youtube.com/*",
]
url_blocklist = []           # optional, applied after allowlist
disable_dev_tools = true
disable_incognito = true
disable_extensions = true
```

### 3. Firewall (issue #4, generic, prerequisite)

A generic `[entries.firewall]` config section, *not* browser-specific.
Enforced by spawning the entry inside a `systemd-run --user --scope` with
the following properties set:

- `IPAddressAllow=` / `IPAddressDeny=` — systemd's BPF firewall, per-scope,
  exactly aligned with the existing per-session model
- `RestrictAddressFamilies=` — drop AF_RAW, AF_PACKET, AF_NETLINK as
  appropriate
- `IPAccounting=yes` — visibility into per-session traffic, optional

Decision: **IP/CIDR allowlists only** for the initial implementation.
Hostname-level rules are not enforced at the kernel layer; they are
delegated to the browser via `URLAllowlist`. This matches what the user
agreed to: "browser policy is the authoritative allowlist, firewall is
coarse." A DNS-resolving helper can be added later if leaks become a
concern.

Schema sketch:

```toml
[entries.firewall]
default = "deny"             # "deny" | "allow"
allow = [
    "127.0.0.0/8",
    "::1/128",
    # Google authentication and Workspace IP ranges
    # (sourced from https://www.gstatic.com/ipranges/goog.json or similar)
]
deny = []                    # applied after allow
```

## Session persistence

Per the user's request, profile persistence is configurable, with the
ability to share a profile across activities or wipe it on exit.

The `profile_id` field under `[entries.browser]` is the persistence key:

- **Persistent + private**: each entry sets its own unique `profile_id`.
  Cookies and logins survive across sessions for that entry only.
- **Persistent + shared**: multiple entries set the same `profile_id` (e.g.
  one "Google Classroom" entry and one "Google Docs" entry both use
  `profile_id = "school"`). They share cookies, so signing in once carries
  over.
- **Ephemeral**: set `wipe_on_exit = true`. The on-disk profile directory
  is removed by the host adapter after the session terminates.

Schema additions:

```toml
[entries.browser]
profile_id = "school"
wipe_on_exit = false  # default; true = clear profile dir after session ends
```

Implementation note: on-exit wiping happens in the Linux host adapter's
post-exit cleanup, not in Chrome. Chrome cannot be trusted to clear its own
state reliably under crash conditions.

## Composition example

A single TOML entry combining all three layers, plus the existing
availability/limits/internet sections:

```toml
[[entries]]
id = "chrome-school"
label = "School"
icon = "com.google.Chrome"

[entries.kind]
type = "flatpak"
app_id = "com.google.Chrome"

[entries.browser]
profile_id = "school"
mode = "kiosk"
start_url = "https://classroom.google.com"
url_allowlist = [
    "https://*.google.com/*",
    "https://*.googleusercontent.com/*",
]
disable_dev_tools = true
disable_incognito = true
disable_extensions = true
wipe_on_exit = false

[entries.firewall]
default = "deny"
allow = [
    # Google IP ranges
]

[entries.availability]
[[entries.availability.windows]]
days = "weekdays"
start = "15:00"
end = "18:00"

[entries.limits]
max_run_seconds = 3600
daily_quota_seconds = 7200
```

## Why this shape

- **Browser-ness is configuration, not a host capability.** Keeps
  `shepherd-host-api` lean and avoids forcing non-Linux hosts (future) to
  understand "browser" as a primitive.
- **#4 becomes generic.** The same firewall section serves the
  "Minecraft → Microsoft auth + selected servers" example called out in
  #4. Browser entries just *use* it.
- **All three layers are independently revocable.** Removing the firewall
  section doesn't break the browser policy; removing the browser section
  leaves a plain Flatpak entry.
- **Aligns with "wrappers, not patches."** No Chrome modification, only
  documented external controls.

## Tradeoffs and known limitations

- **DNS-based allowlists are imperfect at the IP layer.** Mitigation: the
  browser's `URLAllowlist` is authoritative for hostname matching;
  systemd's BPF firewall is defense-in-depth and blocks raw-socket
  exfiltration. If this proves leaky in practice, revisit with a DNS
  helper.
- **Google's IP ranges drift.** A user-maintained allowlist will rot. A
  follow-up issue should consider periodic refresh from
  `https://www.gstatic.com/ipranges/goog.json` (and the Microsoft
  equivalent), but that is out of scope for #10.
- **Flatpak Chrome's Widevine is supported but not first-party.** The
  Flatpak packaging fetches Widevine separately. Acceptable for stated
  use cases.
- **Snap Chromium is rejected** because it is a different browser
  (Chromium, not Chrome) and does not satisfy the "Chrome proprietary"
  requirement in #10.

## Suggested implementation order

1. **#4 first**: land `[entries.firewall]` as a generic mechanism enforced
   via `systemd-run --user --scope` properties. Validate end-to-end with
   an existing entry (e.g. the Minecraft Flatpak entry) before touching
   the browser path.
2. **`[entries.browser]` schema** in `shepherd-config`: types, validation,
   round-trip through `RawEntry` → `Entry`.
3. **Policy JSON materialization** in `shepherd-host-linux`: write the
   `~/.var/app/com.google.Chrome/config/chromium/policies/managed/<entry_id>.json`
   file before each spawn, including kiosk/app-mode flags as Chrome
   command-line args.
4. **Profile management**: per-entry user-data-dir, optional `wipe_on_exit`
   cleanup in the post-exit path of the Linux host adapter.
5. **Documentation**: extend [config.example.toml](../../../config.example.toml)
   with a school-mode browser entry combining all three layers, and update
   [crates/shepherd-config/README.md](../../../crates/shepherd-config/README.md).

## Touch points

Likely files to change (subject to revision during implementation):

- [crates/shepherd-config/src/schema.rs](../../../crates/shepherd-config/src/schema.rs) — new `RawBrowserConfig`, `RawFirewallConfig`
- [crates/shepherd-config/src/policy.rs](../../../crates/shepherd-config/src/policy.rs) — validated policy types
- [crates/shepherd-config/src/validation.rs](../../../crates/shepherd-config/src/validation.rs) — CIDR parsing, URL pattern validation
- [crates/shepherd-host-linux/src/process.rs](../../../crates/shepherd-host-linux/src/process.rs) — wrap spawn with `systemd-run --user --scope` when firewall config is present
- [crates/shepherd-host-linux/src/adapter.rs](../../../crates/shepherd-host-linux/src/adapter.rs) — policy-JSON write, profile-dir wipe on exit
- [config.example.toml](../../../config.example.toml) — example entry
- [crates/shepherd-config/README.md](../../../crates/shepherd-config/README.md) — schema docs

## Decisions confirmed with user (2026-05-01)

- Flatpak Chrome is the only supported browser path. No firejail, no
  native `.deb`.
- IP/CIDR allowlists only for the firewall. Hostname matching is delegated
  to the browser's `URLAllowlist`. No DNS helper in v1.
- Profile persistence is configurable: `profile_id` selects the on-disk
  profile (shareable across entries), `wipe_on_exit` controls per-session
  wiping.

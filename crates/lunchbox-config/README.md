# lunchbox-config

Configuration parsing and validation for Lunchbox.

## Overview

This crate handles loading, parsing, and validating the TOML configuration that defines what entries are available, when they're available, and for how long. It provides:

- **Schema definitions** - Raw configuration structure as parsed from TOML
- **Policy objects** - Validated, ready-to-use policy structures
- **Validation** - Detailed error messages for misconfiguration
- **Hot reload support** - Configuration can be reloaded at runtime

## Configuration Format

Lunchbox uses TOML for configuration. Here's a complete example:

```toml
config_version = 1

[service]
socket_path = "/run/lunchboxd/lunchboxd.sock"
data_dir = "/var/lib/lunchboxd"
default_max_run_seconds = 1800  # 30 minutes default
cooldown_min_session_seconds = 120  # sessions shorter than this skip the cooldown
save_grace_seconds = 120  # time to save when a resume lands after the hours

# Internet connectivity check (optional)
[service.internet]
check = "https://connectivitycheck.gstatic.com/generate_204"
interval_seconds = 10
timeout_ms = 1500

# Global volume restrictions
[service.volume]
max_volume = 80
allow_unmute = true

# Default warning thresholds (seconds before expiry)
[[service.default_warnings]]
seconds_before = 300  # 5 minutes
severity = "info"

[[service.default_warnings]]
seconds_before = 60   # 1 minute
severity = "warn"

[[service.default_warnings]]
seconds_before = 10
severity = "critical"
message_template = "Closing in {remaining} seconds!"

# Entry definitions
[[entries]]
id = "minecraft"
label = "Minecraft"
icon = "minecraft"
kind = { type = "snap", snap_name = "mc-installer" }

[entries.internet]
required = true

[entries.availability]
[[entries.availability.windows]]
days = "weekdays"
start = "15:00"
end = "18:00"

[[entries.availability.windows]]
days = "weekends"
start = "10:00"
end = "20:00"

[entries.limits]
max_run_seconds = 1800       # 30 minutes per session
daily_quota_seconds = 7200   # 2 hours per day
cooldown_seconds = 600       # 10 minutes between sessions

# Token gate (issue #8): this entry has to be earned by playing others first.
# Their sessions bank time here; this entry's sessions spend it back down.
[entries.tokens]
from = ["educational-game", "typing-tutor"]
earn_ratio = 0.5             # 2 minutes earned banks 1 minute here
minimum_seconds = 600        # don't unlock for less than 10 minutes
max_balance_seconds = 3600   # never bank more than an hour (0 = unlimited)
carry_over = false           # unspent time expires at local midnight

[[entries]]
id = "educational-game"
label = "GCompris"
icon = "gcompris-qt"
kind = { type = "process", command = "gcompris-qt" }

[entries.availability]
always = true  # Always available

[entries.limits]
max_run_seconds = 3600  # 1 hour
```

## Usage

### Loading Configuration

```rust
use lunchbox_config::{load_config, parse_config, Policy};
use std::path::Path;

// Load from file (typically ~/.config/lunchbox/config.toml)
let policy = load_config("config.toml")?;

// Parse from string
let toml_content = std::fs::read_to_string("config.toml")?;
let policy = parse_config(&toml_content)?;

// Access entries
for entry in &policy.entries {
    println!("{}: {:?}", entry.label, entry.kind);
}
```

### Entry Kinds

Entries can be of several types:

```toml
# Regular process
kind = { type = "process", command = "/usr/bin/game", args = ["--fullscreen"] }

# Snap application
kind = { type = "snap", snap_name = "mc-installer" }

# Steam game (via Steam snap)
kind = { type = "steam", app_id = 504230 }

# Virtual machine (future)
kind = { type = "vm", driver = "qemu", args = { disk = "game.qcow2" } }

# Media library, played by `lunchbox-media` (docs/lunchbox-media.md)
kind = { type = "media", library = "~/.config/lunchbox/movies.toml" }
kind = { type = "media", library = "~/.config/lunchbox/movies.toml", mode = "play", item = "big-buck-bunny" }

# Emulated game, played through RetroArch. Saves state on close and restores
# it on open; `content` must be absolute or start with `~/`. The HUD offers a
# reset ("reboot the console") button unless `reset = false` — with save-state
# resume on, it is the only way back to a game's own title screen.
kind = { type = "retroarch", core = "mgba", content = "~/Games/roms/game.gba" }
kind = { type = "ebook", book = "~/Books/the-hobbit.epub" }

# Custom type
kind = { type = "custom", type_name = "my-launcher", payload = { ... } }
```

### Time Windows

Time windows control when entries are available:

```toml
[entries.availability]
[[entries.availability.windows]]
days = "weekdays"        # or "weekends", "all"
start = "15:00"
end = "18:00"

[[entries.availability.windows]]
days = ["sat", "sun"]    # Specific days
start = "09:00"
end = "21:00"
```

### Limits

Control session duration and frequency:

```toml
[entries.limits]
max_run_seconds = 1800        # Max duration per session
daily_quota_seconds = 7200    # Total daily limit
cooldown_seconds = 600        # Wait time between sessions
cooldown_min_session_seconds = 120  # sessions shorter than this skip the cooldown
save_grace_seconds = 120      # time to save when a resume lands after the hours
```

`cooldown_min_session_seconds` is a workaround for unstable activities: a
session that ends before it elapses leaves the cooldown untouched, so an
activity that crashes seconds after launch doesn't lock the child out of
something they never got to play. It defaults to
`service.cooldown_min_session_seconds` (itself 120 by default), and `0` restores
the plain behaviour of cooling down after every session however short. Groups
take the same key in `[groups.limits]` and apply it to the group cooldown
independently of their members' own settings.

`save_grace_seconds` (issue #155) covers the case where the device sleeps
mid-session and wakes after the activity's hours have passed. The session clock
is monotonic, so sleeping never spends a child's time — but it does carry a
session past the wall-clock window that bounded it, and an overnight sleep would
otherwise leave a bedtime activity running the next morning. On waking outside
its hours the session is clamped to this long, with a warning, instead of being
cut off mid-sentence; `0` closes it as soon as the device wakes.

It is the one limit that genuinely **cascades** rather than being evaluated at
both levels — an entry's own value, else its group's, else
`service.save_grace_seconds` (itself 120 by default). A session belongs to one
activity, so there is no sense in which an entry and its group could each be held
to their own answer, and the entry's resolved value is used even when it was the
*group's* window that closed: the child is saving the activity in front of them
either way.

### Token Gates

An entry with `[entries.tokens]` (issue #8) has to be *earned*: sessions on the
activities listed in `from` bank a balance on it, and its own sessions spend that
balance back down.

```toml
[entries.tokens]
from = ["educational-game", "typing-tutor"]
earn_ratio = 0.5             # 2 minutes earned banks 1 minute here
minimum_seconds = 600        # don't unlock for less than 10 minutes
max_balance_seconds = 3600   # never bank more than an hour (0 = unlimited)
carry_over = false           # unspent time expires at local midnight
```

Balances move only at session end, the same moment usage is recorded — nothing
updates mid-session.

### Groups

A group (issue #5) is a category of activities that share one schedule and one
budget. Entries join with `group = "<id>"`. Groups are also the categories the
launcher draws as compartments, in declaration order (issue #207), so a group
is worth declaring for the sorting alone — `id` and `label` are the only
required fields.

```toml
[[groups]]
id = "play"
label = "Play"

[groups.availability]
[[groups.availability.windows]]
days = "weekends"
start = "10:00"
end = "18:00"

[groups.limits]
max_run_seconds = 900        # short bursts, per session, for any member
daily_quota_seconds = 3600   # COMBINED across all members
cooldown_seconds = 600       # any member's session cools down the whole group
cooldown_min_session_seconds = 120  # unless that session was shorter than this
save_grace_seconds = 300     # members fall back to this unless they set their own

# A group can be token-gated too: earning unlocks every member at once.
[groups.tokens]
from = ["educational-game"]

[[entries]]
id = "some-game"
group = "play"
```

A token gate's `from` accepts group IDs prefixed with `group:`, so a whole
category can be the *source* of earned time as well as its destination:

```toml
[entries.tokens]
from = ["group:educational"]   # any member of that category banks time
```

Groups are also limit subjects for daily overrides, so a caregiver can enable or
disable a whole category for the day with a single call by passing `group:play`
as the override id.

### How the limits interact

The restrictions compose on two independent axes. **Visibility** is a plain AND:
an entry appears only if it passes every check, and each failure contributes its
own `ReasonCode`. **Session length** is the minimum of every applicable cap.

Every limit exists at both levels — on the entry, and on its group — and the
strictest of each wins. A group-level failure is reported as `GroupRestricted`
wrapping the underlying reason, so the UI can say "Games: daily limit reached".

| | Hides the entry | Caps the session | `availability = true` override bypasses | Also at group level |
| --- | --- | --- | --- | --- |
| Availability window | yes | yes | yes | yes |
| Daily quota | yes | yes | yes | yes (combined across members) |
| Cooldown | yes | — | **no** | yes (any member starts it for all) |
| Token gate | yes | yes | yes | yes (unlocks all members) |
| `max_run` | — | yes | no (only the daily quota is lifted) | yes |

A token-gated entry is therefore **never unlimited**, even with
`max_run_seconds = 0` and no service default: the banked balance always caps it.
The same is true of a member of a token-gated group.

Things to watch for when combining a token gate with the other limits:

- **A daily quota can strand earned tokens.** If the balance is healthy but the
  entry's `daily_quota_seconds` is spent, the entry is hidden with
  `QuotaExhausted` and the banked time cannot be spent — and with
  `carry_over = false` it expires at midnight. The child did the work and the
  reward disappeared. **Prefer not to set `daily_quota_seconds` on a token-gated
  entry at all**: the gate is already the budget, and a quota on top is a second,
  invisible one. If you want both, set `carry_over = true` so earned time
  survives to the next day.
- **Availability windows strand tokens the same way** — time earned after the
  entry's window has closed can't be spent that day.
- **A source's own quota caps how much can be earned.** Time on a source
  activity still counts against that activity's `daily_quota_seconds`. That is a
  reasonable implicit ceiling on daily earning, but it is easy to set by accident
  and then wonder why earning stopped.
- **Set `minimum_seconds`.** It defaults to 0, so any balance above zero unlocks
  the entry — a 20-second balance buys a 20-second session. Warnings whose
  `seconds_before` exceeds the session length are skipped, so such a session ends
  with no countdown at all.
- **A caregiver can grant time directly.** `adjust_tokens {"id": "<subject>",
  "delta_seconds": 600}` banks time on an entry or a `group:<id>`, and a
  negative delta takes it back. Granted time is indistinguishable from earned
  time: capped by `max_balance_seconds`, spent by the gated activity's sessions,
  and it opens the gate only once the balance reaches `minimum_seconds`. To
  switch an activity on regardless of its balance, use an availability override.
  Both management apps expose this as a ±5 min stepper beside the balance.
- **`minimum_seconds` has to be banked every time, not just once.** The
  activity is locked whenever the balance is below it (issue #193). The minimum
  is what guarantees a session long enough to be worth starting — a whole
  battle, a whole level — so a gate that stayed open below it would hand out
  exactly the short sessions it exists to prevent. A session that does start
  can spend the *whole* balance, so nothing is cut off at the threshold. With
  `minimum_seconds = 600` and 700 s banked, a 5-minute session leaves 400 s:
  the activity locks, the 400 s stays banked, and another 200 s earned opens it
  again onto all 600 s. With `carry_over = false` a remainder that is never
  topped up expires at midnight like any other balance.
- **Cooldowns stack on both ends**: a gated entry still cools down after
  spending, and a cooldown on a *source* throttles the rate of earning.

Time is deducted for the wall-clock actually played, whichever cap ended the
session — if a window closes early, the unspent balance stays banked.

And when combining groups with the rest:

- **Group quota is consumed by whichever member is played**, so one activity can
  burn the whole category's budget and take its siblings down with it. That is
  the point of the feature, but it surprises people the first time.
- **A group cooldown is the reason to use groups for cooldowns at all** — a
  per-entry cooldown is trivially dodged by starting a different game in the same
  category.
- **The short-session grace is dodgeable on purpose.** A child who quits every
  activity just under `cooldown_min_session_seconds` never triggers a cooldown;
  the daily quota is what still bounds them. Lower it (or set it to 0) on
  activities that are stable enough not to need the workaround.
- **Entries with no `group` are completely unaffected** by any of this.
- **Overrides work at both levels.** A group override enables or disables every
  member with one call, and a force-enable on *either* the entry or its group
  lifts the entry's own limits too: enabling a category for the day means its
  activities are on today, whatever their individual schedules say.
- **The token cautions above apply at group level, more sharply.** A group quota
  can strand time earned toward a whole category.
- **Avoid gating an entry and its group.** An entry that is token-gated *and*
  sits in a token-gated group spends *both* balances for one session. It is
  coherent — two budgets, both paid — but it is hard to explain to a child. Gate
  at one level or the other.

### Internet Requirements

Entries can require internet connectivity. When the device is offline, those entries are hidden.

```toml
[service.internet]
check = "https://connectivitycheck.gstatic.com/generate_204"
interval_seconds = 300
timeout_ms = 1500

[entries.internet]
required = true
# Optional per-entry override:
# check = "tcp://1.1.1.1:53"
```

In addition to the `interval_seconds` poll, connectivity is re-checked
immediately when the machine resumes from suspend (logind `PrepareForSleep`) and
when a network adapter changes state (NetworkManager), so status reflects
reality without waiting for the next interval.

### Firewall

Entries may apply a network allowlist/denylist enforced via systemd's BPF
address filter (`IPAddressAllow=`/`IPAddressDeny=`). Rules are IP addresses,
CIDR ranges, or systemd tokens (`any`, `localhost`, `link-local`,
`multicast`). Hostnames are **not** resolved at this layer — pair with a
browser-side allowlist (e.g. Chrome `URLAllowlist`) when hostname matching
is required.

```toml
[entries.firewall]
default = "deny"   # "deny" (default) or "allow"
allow = [
    "127.0.0.0/8",
    "::1/128",
    "10.0.0.0/8",
]
deny = []
```

Enforcement notes:
- For `process` entries, the session is wrapped in a transient
  `systemd-run --user --scope` with the firewall properties set up front.
- For `flatpak` and `snap` entries, the runtime creates its own scope; the
  firewall is applied via `systemctl --user --runtime set-property` once
  that scope appears (small race window during early app startup).
- Not yet supported for `steam` entries.

#### Firewall caveats: single-instance apps

The firewall applies to the cgroup of the launched activity. Programs that
implement the single-instance / "open in existing window" pattern via D-Bus
registration (most modern GTK/GApplication apps -- `ptyxis`,
`gnome-terminal`, `nautilus`, `evince`, etc., plus browsers via their own
remote-control protocol) **escape the scope**: the binary lunchboxd launches
forwards the request to a long-lived primary in `user@.service`, exits in
~50ms, the scope is torn down, and the visible window is forked by the
primary in a cgroup the BPF program is not attached to. The firewall block
is silently a no-op.

Workarounds:
- Prefer non-daemonising alternatives (e.g. `foot` instead of `ptyxis`,
  `xterm` instead of `gnome-terminal`).
- For ptyxis specifically, `ptyxis -s` / `--standalone` runs the terminal
  in-process and inherits the scope correctly.
- Chromium/Firefox accept `--new-instance` / equivalent; verify with
  `cat /proc/$$/cgroup` from inside the app that it sits under the
  expected `user.slice/.../*.scope`.

### Browser

Entries may carry a supervised-browser policy that wraps Chrome through
documented controls only — a Chromium [enterprise-policy][policies] JSON file
plus Chrome command-line flags, materialized at spawn time. It is a
*composition* layer: pair it with `kind = { type = "flatpak", app_id =
"com.google.Chrome" }` (the sandboxed Chrome) and an optional
`[entries.firewall]` block. There is no dedicated browser entry kind.

```toml
[entries.browser]
profile_id = "school"        # on-disk user-data-dir segment (shareable)
mode = "kiosk"               # "kiosk" (default) | "app" | "windowed"
start_url = "https://classroom.google.com"
url_allowlist = ["https://*.google.com/*"]
url_blocklist = []           # applied after the allowlist
disable_dev_tools = true     # default true
disable_incognito = true     # default true
disable_extensions = true    # default true
wipe_on_exit = false         # default false
```

[policies]: https://chromeenterprise.google/policies/

Notes:
- `profile_id` is the persistence key. Entries sharing an id share
  cookies/logins; each unique id is isolated. It must be a single safe path
  segment (ASCII letters, digits, `-`, `_`, `.`; not `.`/`..`).
- Hostname allowlisting is enforced by Chrome via `URLAllowlist`/`URLBlocklist`
  (no extensions). The firewall is coarse IP-layer defense-in-depth.
- `url_allowlist`/`url_blocklist` entries use Chrome's [URL-filter format][urlf]
  and should be **scheme-qualified** (`https://host/...`): a bare `host` or
  `host:port` is not reliably matched and would be caught by the authoritative
  catch-all block.

[urlf]: https://chromeenterprise.google/policies/url-blocking/
- `wipe_on_exit` clears the profile directory in the host adapter's post-exit
  cleanup, not in Chrome.
- Validation rejects unknown `mode`, non-http(s) `start_url`, empty/whitespace
  URL patterns, and unsafe `profile_id` values.

## Defaults, and who else needs to know them

A field left out of `config.toml` gets its default one of two ways, and the
difference matters to anything outside this crate that has to predict what the
daemon will do — the web config editor above all, which renders an unset control
as the value it will actually take.

**Serde defaults** (`#[serde(default = "…")]` in `schema.rs`) are applied at
deserialization. `schemars` reads the attribute and writes the value into the
JSON Schema, so they travel out of the crate on their own.

**Load-time defaults** are `Option<T>` fields whose `None` means "fall back",
resolved in `Policy::from_raw`. `schemars` sees only `"default": null` for
these, so they travel through
[`LoadTimeDefaults`](src/load_defaults.rs) instead — a struct that exists purely
so `lunchbox-wire-codegen` can enumerate them, carrying `default_max_run_seconds`,
the two cooldown/save-grace fallbacks, the Steam launch timeout, the internet
check interval and timeout, the management API's port, bind and bind-retry, the
token earn ratio, the default HUD edge, and the default warning schedule.

Adding a load-time default means adding a field there as well, or the editor
goes back to guessing. The module's own test parses a config that sets none of
them and asserts what it advertises is what the parser produces; see
`CONTRIBUTING.md` for how the generated file is refreshed and checked.

## Validation

The configuration is validated at load time. Validation catches:

- **Duplicate entry IDs** - Each entry must have a unique ID
- **Empty commands** - Process entries must specify a command
- **Invalid time windows** - Start time must be before end time
- **Invalid thresholds** - Warning thresholds must be less than max run time
- **Negative durations** - All durations must be positive
- **Unknown kinds** - Entry types must be recognized (unless Custom)

```rust
use lunchbox_config::{parse_config, ConfigError};

let result = parse_config(toml_str);
match result {
    Ok(policy) => { /* Use policy */ }
    Err(ConfigError::ValidationFailed { errors }) => {
        for error in errors {
            eprintln!("Config error: {}", error);
        }
    }
    Err(e) => eprintln!("Failed to load config: {}", e),
}
```

## Hot Reload

Configuration can be reloaded at runtime via the service's `ReloadConfig` command or by sending `SIGHUP` to the service process. Reload is atomic: either the new configuration is fully applied or the old one remains.

Active sessions continue with their original time limits when configuration is reloaded.

## Key Types

- `Policy` - Validated policy ready for the core engine
- `Entry` - A launchable entry definition
- `AvailabilityPolicy` - Time window rules
- `LimitsPolicy` - Duration and quota limits
- `WarningPolicy` - Warning threshold configuration
- `VolumePolicy` - Volume restrictions

## Design Philosophy

- **Human-readable** - TOML is easy to read and write
- **Strict validation** - Catch errors at load time, not runtime
- **Versioned schema** - `config_version` enables future migrations
- **Sensible defaults** - Minimal config is valid

## Dependencies

- `toml` - TOML parsing
- `serde` - Deserialization
- `chrono` - Time types
- `thiserror` - Error types

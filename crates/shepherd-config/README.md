# shepherd-config

Configuration parsing and validation for Shepherd.

## Overview

This crate handles loading, parsing, and validating the TOML configuration that defines what entries are available, when they're available, and for how long. It provides:

- **Schema definitions** - Raw configuration structure as parsed from TOML
- **Policy objects** - Validated, ready-to-use policy structures
- **Validation** - Detailed error messages for misconfiguration
- **Hot reload support** - Configuration can be reloaded at runtime

## Configuration Format

Shepherd uses TOML for configuration. Here's a complete example:

```toml
config_version = 1

[service]
socket_path = "/run/shepherdd/shepherdd.sock"
data_dir = "/var/lib/shepherdd"
default_max_run_seconds = 1800  # 30 minutes default

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
use shepherd_config::{load_config, parse_config, Policy};
use std::path::Path;

// Load from file (typically ~/.config/shepherd/config.toml)
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

# Media playback (future)
kind = { type = "media", library_id = "movies" }

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
```

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
budget. Entries join with `group = "<id>"`.

```toml
[[groups]]
id = "attention-heavy"
label = "Games"

[groups.availability]
[[groups.availability.windows]]
days = "weekends"
start = "10:00"
end = "18:00"

[groups.limits]
max_run_seconds = 900        # short bursts, per session, for any member
daily_quota_seconds = 3600   # COMBINED across all members
cooldown_seconds = 600       # any member's session cools down the whole group

# A group can be token-gated too: earning unlocks every member at once.
[groups.tokens]
from = ["educational-game"]

[[entries]]
id = "some-game"
group = "attention-heavy"
```

A token gate's `from` accepts group IDs prefixed with `group:`, so a whole
category can be the *source* of earned time as well as its destination:

```toml
[entries.tokens]
from = ["group:educational"]   # any member of that category banks time
```

Groups are also limit subjects for daily overrides, so a caregiver can enable or
disable a whole category for the day with a single call by passing
`group:attention-heavy` as the override id.

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
- **`minimum_seconds` is a threshold to cross, not one to stay above.** Once the
  balance reaches it the gate ratchets open and stays open until the balance is
  spent to zero, so a short session doesn't re-lock the activity and strand the
  rest. With `minimum_seconds = 600` and 700 s banked, a 5-minute session leaves
  400 s that are still spendable. Spending the balance out closes the gate again,
  and the threshold has to be crossed from zero.
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
remote-control protocol) **escape the scope**: the binary shepherdd launches
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

## Validation

The configuration is validated at load time. Validation catches:

- **Duplicate entry IDs** - Each entry must have a unique ID
- **Empty commands** - Process entries must specify a command
- **Invalid time windows** - Start time must be before end time
- **Invalid thresholds** - Warning thresholds must be less than max run time
- **Negative durations** - All durations must be positive
- **Unknown kinds** - Entry types must be recognized (unless Custom)

```rust
use shepherd_config::{parse_config, ConfigError};

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

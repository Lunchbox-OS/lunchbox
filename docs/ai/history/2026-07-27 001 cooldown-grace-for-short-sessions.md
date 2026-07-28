# Cooldown grace period for short sessions (unstable-activity workaround)

## Prompt

> make it so that the cooldowns don't take effect if an activity stops after
> some amount of time (default 2 minutes)
>
> as a workaround for unstable activities

## Problem

`settle_session_end` in `shepherd-core` starts the configured cooldown for the
entry and its group whenever a session ends, however short that session was. An
activity that crashes or bounces seconds after launch therefore starts a full
cooldown, and the child is locked out of something they never got to play. With
a group cooldown the blast radius is the whole category.

## Approach

A per-subject grace period: a session shorter than `cooldown_min_session` leaves
the cooldown alone.

- `service.cooldown_min_session_seconds` sets the default (`120`, the constant
  `shepherd_config::policy::DEFAULT_COOLDOWN_MIN_SESSION`).
- `limits.cooldown_min_session_seconds` overrides it per entry and per group.
  `0` restores the old behaviour (every session cools down).
- Resolved at config load time in `convert_limits`, so `LimitsPolicy` carries a
  plain `Duration` and the engine reads one field.
- Entry and group thresholds are evaluated separately in `settle_session_end`,
  each against the same session duration. A group can forgive a crash that the
  member itself still cools down for, and vice versa. The skip is logged at
  `info` with the subject and both durations.

Only cooldowns are affected. Usage accounting and token earn/spend still count
the seconds actually played — a crashed session bills for the few seconds it ran,
which is what it did before.

### Trade-off accepted

The grace is dodgeable by design: quitting just under the threshold every time
never triggers a cooldown. The daily quota is what still bounds that, and the
knob is per subject, so activities stable enough not to need the workaround can
set it to `0`. Documented as a gotcha in `crates/shepherd-config/README.md`.

## Files touched

- `crates/shepherd-config/src/schema.rs` — `RawServiceConfig` and `RawLimits`
  keys.
- `crates/shepherd-config/src/policy.rs` — `LimitsPolicy.cooldown_min_session`,
  the default constant, threading through `Entry::from_raw` / `Group::from_raw`
  / `convert_limits`.
- `crates/shepherd-core/src/engine.rs` — the skip in `settle_session_end`, plus
  four tests (entry grace, zero grace, group grace, entry/group independence).
- `crates/shepherd-config/src/lib.rs` — config-level tests for the default and
  the two override levels.
- Docs: `config.example.toml`, `README.md`, `crates/shepherd-config/README.md`,
  `crates/shepherd-core/README.md`, `crates/shepherdd/README.md`.

## Verification

- `cargo test` across the touched crates.
- `cargo run -p shepherd-config --bin validate-config -- config.example.toml`.
- `cargo fmt --all`, `cargo clippy --all-targets`.

Not exercised end-to-end on a real crashing activity; the behaviour is covered
by engine tests that run a 20-second session against a 120-second grace period.

## Environment note

The dev box was at 100 % disk (`/` full, 177 MB free) and `cargo build` failed
with "No space left on device". Clearing `target/debug/incremental` (9.8 GB, a
pure cache cargo regenerates) freed enough to build; `target/debug/deps` was
still 24 GB afterwards, so a `cargo clean` may be due.

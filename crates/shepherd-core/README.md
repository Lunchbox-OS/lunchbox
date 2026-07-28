# shepherd-core

Core policy engine and session state machine for Shepherd.

## Overview

This crate is the heart of Shepherd, containing all policy evaluation and session management logic. It is completely platform-agnostic and makes no assumptions about the underlying operating system or display environment.

### Responsibilities

- **Policy evaluation** - Determine what entries are available, when, and for how long
- **Session lifecycle** - Manage the state machine from launch to termination
- **Warning scheduling** - Compute and emit warnings at configured thresholds
- **Time enforcement** - Track deadlines using monotonic time
- **Quota management** - Track daily usage and cooldowns

## Session State Machine

Sessions progress through the following states:

```
              ┌─────────────┐
              │   Idle      │ (no session)
              └──────┬──────┘
                     │ Launch requested
                     ▼
              ┌─────────────┐
              │  Launching  │
              └──────┬──────┘
                     │ Process spawned
                     ▼
              ┌─────────────┐
     ┌───────▶│   Running   │◀──────┐
     │        └──────┬──────┘       │
     │               │ Warning threshold
     │               ▼
     │        ┌─────────────┐
     │        │   Warned    │ (multiple levels)
     │        └──────┬──────┘
     │               │ Deadline reached
     │               ▼
     │        ┌─────────────┐
     │        │  Expiring   │ (termination in progress)
     │        └──────┬──────┘
     │               │ Process ended
     │               ▼
     │        ┌─────────────┐
     └────────│   Ended     │───────▶ (return to Idle)
              └─────────────┘
```

## Key Types

### CoreEngine

The main policy engine:

```rust
use shepherd_core::CoreEngine;
use shepherd_config::Policy;
use shepherd_host_api::HostCapabilities;
use shepherd_store::Store;
use std::sync::Arc;

// Create the engine
let engine = CoreEngine::new(
    policy,                   // Loaded configuration
    store,                    // Persistence layer
    host.capabilities().clone(), // What the host can do
);

// List entries with current availability
let entries = engine.list_entries(Local::now());

// Request to launch an entry
match engine.request_launch(&entry_id, Local::now()) {
    LaunchDecision::Approved(plan) => {
        // Spawn via host adapter, then start session
        engine.start_session(plan, host_handle, MonotonicInstant::now());
    }
    LaunchDecision::Denied { reasons } => {
        // Cannot launch, explain why
    }
}
```

### Session Plan

When a launch is approved, the engine computes a complete session plan:

```rust
pub struct SessionPlan {
    pub session_id: SessionId,
    pub entry_id: EntryId,
    pub entry: Entry,
    pub started_at: DateTime<Local>,
    /// None means unlimited (no time limit)
    pub deadline: Option<MonotonicInstant>,
    pub warnings: Vec<ScheduledWarning>,
}
```

The plan is computed once at launch time. Deadlines and warnings are deterministic.

### Events

The engine emits events for the IPC layer and host adapter:

```rust
pub enum CoreEvent {
    // Session lifecycle
    SessionStarted { session_id, entry_id, deadline },
    Warning { session_id, threshold_secs, remaining, severity, message },
    ExpireDue { session_id },
    SessionEnded { session_id, reason },
    
    // Policy
    PolicyReloaded { entry_count },
}
```

### Tick Processing

The engine must be ticked periodically to check for warnings and expiry:

```rust
// In the service main loop
let events = engine.tick(MonotonicInstant::now());
for event in events {
    match event {
        CoreEvent::Warning { .. } => { /* Notify clients */ }
        CoreEvent::ExpireDue { .. } => { /* Terminate session */ }
        // ...
    }
}
```

## Time Handling

The engine uses two time sources:

1. **Wall-clock time** (`DateTime<Local>`) - For availability windows and display
2. **Monotonic time** (`MonotonicInstant`) - For countdown enforcement

This separation ensures:
- Availability follows the user's local clock (correct behavior for "3pm-6pm" windows)
- Session enforcement cannot be bypassed by changing the system clock

```rust
// Availability uses wall-clock
let is_available = entry.availability.is_available(&Local::now());

// Countdown uses monotonic
let remaining = session.time_remaining(MonotonicInstant::now());
```

## Policy Evaluation

For each entry, the engine evaluates:

1. **Explicit disable** - Entry may be disabled in config
2. **Host capabilities** - Can the host run this entry kind?
3. **Time window** - Is "now" within an allowed window?
4. **Active session** - Is another session already running?
5. **Cooldown** - Has enough time passed since the last session?
6. **Daily quota** - Is there remaining quota for today?
7. **Token gate** - Has enough time been earned on this entry's source activities?
8. **Group restrictions** - Every check above, repeated for the entry's group

Each check that fails adds a `ReasonCode` to the entry view, allowing UIs to explain unavailability.

## Token System

An entry with an `[entries.tokens]` gate (issue #8) has to be *earned*: sessions
on its `from` activities bank a balance, which the entry's own sessions spend
back down. `compute_max_duration` caps a session at the banked balance, so it can
never be overspent, and `settle_tokens` moves the balance at session end — the
same point where usage is recorded.

A force-enable daily override bypasses the gate and the cap, and a session run
under that override does not spend the balance: the caregiver granted that time,
so it isn't billed to the child.

## Groups

A group (issue #5) carries the same limits an entry does — window, quota,
`max_run`, cooldown, token gate — shared by every member. Evaluation applies both
levels and the strictest of each wins; a group-level failure is reported as
`GroupRestricted` wrapping the underlying reason.

The group daily quota is the *combined* usage of its members, summed from
`Store::get_all_usage_for_date`, so one activity can spend the category's whole
budget. A group cooldown is started by any member's session and applies to all of
them, which is what stops a child hopping between activities to dodge it.

A session shorter than the subject's `cooldown_min_session` (default two minutes)
does not start its cooldown at all — a workaround for unstable activities, which
would otherwise crash on launch and leave the child locked out of something they
never got to play. Entry and group thresholds are evaluated separately, so a
category can forgive a crash that the activity itself still cools down for.

Cooldowns, token balances, and daily overrides are keyed by `LimitSubject`, so a
group holds the same state an entry does — including its own daily override,
which enables or disables every member at once.

## Design Philosophy

- **Determinism** - Given the same inputs, the engine produces the same outputs
- **Platform agnosticism** - No OS-specific code
- **Authority** - The engine is the single source of truth for policy
- **Auditability** - All decisions can be explained via reason codes

## Testing

The engine is designed for testability:

```rust
#[test]
fn test_time_window_evaluation() {
    // Create engine with mock store
    // Set specific time
    // Verify entry availability
}

#[test]
fn test_warning_schedule() {
    // Launch with known deadline
    // Tick at specific times
    // Verify warnings emitted at correct thresholds
}
```

## Dependencies

- `chrono` - Date/time handling
- `shepherd-api` - Shared types
- `shepherd-config` - Policy definitions
- `shepherd-host-api` - Capability types
- `shepherd-store` - Persistence trait
- `shepherd-util` - ID and time utilities

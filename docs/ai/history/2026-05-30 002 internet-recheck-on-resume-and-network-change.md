# Re-run internet checks on resume-from-suspend and network-adapter changes

Date: 2026-05-30

## Prompt

> Make the internet checks run again on resume from suspend and on changes to
> the status of the network adapters

## Problem

shepherdd's internet connectivity monitor (`crates/shepherdd/src/internet.rs`,
`InternetMonitor::run`) only re-checked on a fixed timer (default 10s, see
`DEFAULT_INTERNET_CHECK_INTERVAL`). After resume from suspend, or when a network
adapter went up/down, connectivity status (and the launch-gating of
internet-required entries) could stay stale for up to a full interval.

## Approach (confirmed with the user)

Subscribe to two **system** D-Bus signals via the `zbus` crate (chosen over a
clock-jump heuristic for resume, and over raw netlink for network changes) and
nudge the existing monitor to re-check immediately. The periodic timer remains
as the fallback.

- Resume: `org.freedesktop.login1.Manager.PrepareForSleep` (fires `start =
  false` on wake).
- Network: `org.freedesktop.NetworkManager.StateChanged` (fires on adapter /
  connectivity transitions).

shepherdd runs inside the kiosk Sway session and can read both signals from the
system bus.

## Changes

- `Cargo.toml` (workspace) + `crates/shepherdd/Cargo.toml`: add
  `zbus = { version = "5", default-features = false, features = ["tokio"] }`
  (tokio feature so it shares the existing runtime instead of pulling async-io),
  plus `tokio-stream` on shepherdd for `StreamExt::next` over zbus signal
  streams.
- New `crates/shepherdd/src/system_events.rs`: `RecheckTrigger` enum, two
  `#[zbus::proxy]` traits (`LogindManager`, `NetworkManager`), and
  `spawn_recheck_watchers(tx)` — a task that connects to the system bus,
  `select!`s over both signal streams, and forwards a trigger on each relevant
  event. It reconnects with a 5s backoff and treats missing D-Bus /
  NetworkManager as non-fatal (periodic checks keep working).
- `crates/shepherdd/src/internet.rs`: `InternetMonitor::run` now takes an
  `mpsc::UnboundedReceiver<RecheckTrigger>` and `tokio::select!`s between the
  interval tick and the recheck channel. A burst of triggers is coalesced
  (`try_recv` drain) into a single re-check, and the interval is `reset()` after
  an event so the cadence restarts from it. The immediate first interval tick is
  now consumed to avoid a double-check right after the initial check.
- `crates/shepherdd/src/main.rs`: `mod system_events;`; in the
  `internet_monitor` spawn block, create the channel, call
  `spawn_recheck_watchers`, and pass the receiver into `monitor.run`. Only
  spawned when an internet monitor exists.
- Doc note in `crates/shepherd-config/README.md`.

## Verification

- `cargo build -p shepherdd`, `cargo clippy -p shepherdd --all-targets -- -D
  warnings`, `cargo test -p shepherdd`, `cargo fmt --all` — all clean.
- Manual (in a Sway session): toggle the adapter with `nmcli networking off/on`
  and `systemctl suspend`/resume; with debug logging, expect "Re-running
  internet checks due to system event" and an `InternetStatusChanged` event
  immediately rather than after `interval_seconds`.

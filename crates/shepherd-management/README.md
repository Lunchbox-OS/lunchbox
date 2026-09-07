# shepherd-management

Transport-agnostic management service for shepherdd.

This crate defines `ManagementService`, the trait that captures every
operation an administrator can perform on a running shepherdd: list
entries, launch / stop / extend sessions, set daily overrides, read
usage analytics, adjust volume and brightness, reload config, list and
act on compositor windows, subscribe to the event stream, etc.

It exists so the HTTP transport (`shepherd-http`) and the planned BLE
transport (`shepherd-ble`, see
`docs/ai/history/2026-06-20 002 ble-management.md`) can share one
implementation of the business logic and call it through the same trait.
Handlers in each transport are reduced to translating their wire format
into trait calls and back.

`DefaultManagementService` is the production implementation; it composes
the existing `CoreEngine`, `Store`, `HostAdapter`, `VolumeController`,
`BrightnessController`, and `HidpiController` collaborators that the
daemon already wires up.

## `WebListenerHandle`

The one thing in here that is not a call into a collaborator: a shared,
cheap-to-clone slot holding what the web management interface is *actually*
doing, written by whoever owns the listener and read by `network_status`
(issue #182).

It lives in this crate rather than in `shepherd-http` because `shepherd-http`
depends on this one — the other direction is a dependency cycle.

It exists because the configured `bind` and `port` are an intention, not an
outcome. The daemon retries a bind whose address does not exist yet
(`service.management_api.bind_retry_seconds`, added for a ZeroTier interface
still coming up at login), a `port = 0` binds to something else entirely, and
a bind that never succeeds previously reached only a log line — on a device
whose web interface is exactly how somebody would have read it.

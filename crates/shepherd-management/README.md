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

## Web management authentication

`webauth.rs` holds the credential store behind the management web UI (issue
#156): the Argon2id password hash, the live browser sessions, and the pending
"approve this browser on your phone" requests. It lives here rather than in
`shepherd-http` because the companion reaches two of those three over BLE —
approving a waiting browser, and setting the password without SSH are both
ordinary trait methods, so `#[management_rpc]` carries them to every transport
and to the Kotlin and TypeScript codegen.

What is *not* here is the login itself. Signing in is a pre-auth HTTP exchange
(`POST /api/v1/auth/login` and friends, in `shepherd-http`); a transport that
has already authenticated its peer, as BLE has by the time a GATT write lands,
has no use for one, and exposing it there would be a second door into the same
room.

The store is synchronous behind a `std::sync::RwLock`, for the same reason
`AdminAuthority` is: the HTTP auth middleware runs per request and must not
await. It persists through `ProtectedFiles` to `web-auth.toml` under the state
custodian, at a uid no activity has — a password hash and a table of live
sessions are exactly the kind of thing the child must not be able to read.
Session tokens are stored hashed even there.

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

## The policy file, and the two methods that are not RPCs

`read_policy` and `write_policy` (issue #185) hand the config editor the
device's `config.toml` and take it back. Everything else on the trait is
`async` and therefore reachable over every transport; these two are
deliberately **synchronous**, which is the whole mechanism by which they are
not:

- `#[management_rpc]` builds a `dispatch_json` arm for each `async` method and
  skips the rest. BLE serves that dispatcher, and its frame cap is 16 KiB
  against a config that is comfortably 50 KB — so a policy on the JSON-RPC
  surface would be a method that exists and cannot work. `shepherd-http`
  reaches these over dedicated routes instead.
- `ProtectedFiles` is a blocking interface anyway: on a device each call is a
  round trip to the state custodian's socket. Callers on an async runtime use
  `spawn_blocking`.

Where they read and write is `policy_files` when the custodian holds the
policy and `config_path` otherwise — the same choice `shepherdd` makes at boot,
in one place so a read and a reload cannot disagree about which file decides
what a child may do. `reload_config` goes through the same helper, which is
what fixed it reloading the zero-entry signpost on every custodial device.

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

### Why there is a User-Agent parser in here

Each session carries a label — "Chrome on Android" — so a parent can tell which
row is the laptop they want to sign out. That label comes from `woothee`, and
the dependency is deliberate: this started as a dozen-line token table and the
table was quietly wrong about ordinary devices. Every browser on iOS is WebKit
underneath and announces itself as `CriOS`/`FxiOS`/`EdgiOS` with no `Chrome/`
token at all, so every iPhone read as Safari; ChromeOS says `CrOS` and never
says `Linux`, so those rows had no platform; Android says `Linux; Android 15`
and means the second half. That set of rules only grows, and none of it is
guessable — it belongs in a dataset somebody else maintains.

`woothee` over the alternatives because it is self-contained: the dataset
compiles in, so there is no `regexes.yaml` to vendor and refresh as
`ua-parser` requires, and no deprecated `serde_yaml` as `uaparser` brings. It
added exactly one entry to `Cargo.lock` — the `regex` family was already in
the tree.

The label is for a person to read and is never an input to a decision: a
User-Agent is whatever the client typed. Two things stay knowingly wrong and
are pinned by tests that say so — a desktop-mode iPad is byte-identical to a
Mac, and Windows 11 sends `Windows NT 10.0` just as Windows 10 does, which is
why the version is dropped rather than repeated as a coin flip.

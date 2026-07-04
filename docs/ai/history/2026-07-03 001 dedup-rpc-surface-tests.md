# Deduplicate HTTP / BLE / IPC RPC-surface tests (#65)

## Prompt

`/remote-control`: "Identify the redundancies, if any, in the HTTP, BLE, and
API tests now that they are all served by the same implementation" —
followed by "go ahead and do the whole test refactor as suggested".

## Context

Since #65, every management transport (HTTP `POST /api/v1/rpc`, BLE GATT,
IPC socket) is a thin adapter over one shared, macro-generated core:
the `ManagementService` trait + `dispatch_json` (method-name matching,
param parsing, `#[rpc(wrap_result = ...)]` shaping), emitted by
`#[management_rpc]` in `shepherd-management-macros`. Each transport adds
only its own error-mapping table:

- HTTP (`handlers/rpc.rs`): `RpcDispatchError`/`ManagementError` → HTTP status + `error` string.
- BLE (`rpc.rs::dispatch_management`): same errors → `ErrorCode`.
- IPC: `dispatch_json` directly (no behavioral tests — relies on the shared layer).

### Redundancy found

1. `shepherd-http/tests/api.rs` (44 tests) was doing double duty: ~11 tests
   genuinely HTTP-specific (bearer auth, status-code mapping) and ~33
   exercising shared business logic (volume clamp/policy, override CRUD,
   session lifecycle, time-window override, usage, reload) that behaves
   identically on every transport. It sat at the HTTP layer only because
   that was the one surface with a full-stack mock harness.
2. `shepherd-ble/src/rpc.rs` (7 tests) re-asserted shared `dispatch_json`
   behavior (health dispatch, `logout` null result, zero-arg param
   parsing, `delete_override` wrap shape) that lives in the generated
   dispatcher, not in BLE's ~15-line adapter.
3. `shepherd-e2e/tests/e2e.rs` `extend_session_via_http` and
   `daily_override_disables_entry` re-asserted pure request/response logic
   already covered by the mock-level suite, adding no real-daemon value.
4. The shared `shepherd-management` crate had **no** behavioral tests of
   its own dispatch/business logic (only the codegen-drift guard).

## Change

- **New `shepherd-management/tests/dispatch.rs` (40 tests)** — drives a real
  `DefaultManagementService` (in-memory SQLite + mock host/volume/brightness)
  through `dispatch_json`. This is the single home for dispatch mechanics
  (unknown method → `MethodNotFound`, missing param → `InvalidParams`,
  zero-arg param parsing, `wrap_result` shaping, null results) and per-method
  business logic (entries, sessions, overrides, usage, volume clamp/policy,
  config reload, time-window override).
- **Thinned `shepherd-http/tests/api.rs` (44 → 13 tests)** — keeps only the
  HTTP-transport concerns: the 7 bearer/admin-authority auth tests, and one
  representative call per status-code arm (200, 404 method-not-found, 400
  invalid-params, 404 not-found, 403 forbidden, 422 unprocessable).
- **Thinned `shepherd-ble/src/rpc.rs` (7 → 3 tests)** — keeps only the
  BLE-specific `RpcDispatchError` → `ErrorCode` mapping arms; deleted the four
  shared-behavior tests. (The `ManagementError` → `ErrorCode` table already has
  its own direct test in `protocol.rs::management_error_maps_codes`.)
- **Thinned `shepherd-e2e/tests/e2e.rs`** — removed the two pure-logic
  duplicates; kept the flows that need a live daemon (boot + SIGTERM + IPC
  ping, real process spawn/reap, SSE expiry timer, on-disk config reload,
  real-socket auth).

Net: shared behavior is tested once at the shared layer; each transport tests
only its own auth/status/error-code/framing glue.

## Verification

- `cargo test -p shepherd-management --test dispatch` → 40 passed
- `cargo test -p shepherd-http --test api` → 13 passed
- `cargo test -p shepherd-ble --lib` → 40 passed
- `cargo test -p shepherd-e2e --no-run` → compiles (e2e tests are `#[ignore]`)
- `cargo fmt --all`, `cargo clippy` on all four crates → clean

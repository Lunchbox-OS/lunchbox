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

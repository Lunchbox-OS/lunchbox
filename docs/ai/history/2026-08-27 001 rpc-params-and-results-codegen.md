# Generating the request half of the RPC protocol

*2026-08-27. Came out of a manual review of #139 (the graphical config editor),
not from an issue.*

## The prompt

While reviewing #139 the question came up: *is there anything else that would
benefit from generation?* A sweep of the client mirrors found four things, of
which the request half of the RPC protocol was by far the largest.

## What was hand-written, and why it mattered

`docs/rpc-schema.json` has always carried `params` — 26 of 42 methods have
them, each with `name`, `required`, and its Rust type — and nothing consumed
it. `rpc_codegen.rs` read only the method name and the `wrap_field` hint, so
both clients spelled the parameter keys themselves:

```kotlin
decode(call("set_audio_output_limits", buildJsonObject {
    put("output_key", JsonPrimitive(outputKey))
    put("max_volume", maxVolume?.let { JsonPrimitive(it) } ?: JsonNull)
}))
```
```ts
call<AudioOutputRecord>("set_audio_output_limits", { output_key, max_volume });
```

Rename `output_key` in the trait and both clients keep compiling, then fail
when someone taps the button. That is the same failure shape that let the
companion's payload mirrors drift twice — a `ReasonCode` short four variants, a
renamed `DailyOverride` field — which is what `wire_schema.rs` was written to
close. The generator simply stopped at the method name.

The web client also hand-wrote every *result* type, including three wrap shapes
(`{ new_deadline }`, `{ deleted }`, `{ entry_count }`) that the generator was
already emitting as `RPC_WRAP_FIELDS` — which nothing imported. Two of its
hand-written types had quietly diverged from generated ones that existed and
were unused: `LaunchWire` duplicated `LaunchOutcome`, and `{ live, ready }`
was a three-field-short copy of `HealthStatus`.

## Why it had not been done

The first read was that this needed a whole parallel mechanism. The schema
records params as *stringified Rust types* (`DateTime<Local>`, `EntryId`,
`Option<u8>`), while `ts_types.rs` and `kotlin_types.rs` render JSON Schema —
`rpc_codegen.rs` said as much, calling params "exposed to future type-mapping
codegen".

That was too pessimistic, and the reviewer caught it: **the mapping table
already existed.** `wire_schema()` returns `$defs` *keyed by Rust type name*,
and the schema's param types are Rust type names. The join is the name itself.
Better still, `wire_schema.rs` had already anticipated this — `window_action`,
`stop_mode` and `display_mode` are rooted in the `WireTypes` struct precisely
because they appear only as RPC parameters, with a comment saying so.

Resolving all 15 param type strings and all 24 result type strings against the
generated `$defs` left only:

| bucket | handling |
|---|---|
| named types (`EntryId`, `LimitSubject`, `WindowAction`, `StopMode`, `DisplayMode`, every result payload) | already in `$defs`; existing renderers, no work |
| primitives (`i64`, `u64`, `u8`, `bool`, `String`, `usize`, `f64`, `()`) | one match |
| `DateTime<Local>`, `NaiveDate` | two arms → the `IsoTimestamp`/`IsoDate` aliases the wire mirror already declares |
| `Option<T>`, `Vec<T>`, one nesting level | a recursive strip |

`EntryId` and `LimitSubject` came along free through nesting, so nothing new
even needed rooting. That is `rust_types.rs`, and it is the whole of the "new
mechanism".

## What shipped

- `crates/shepherd-wire-codegen/src/rust_types.rs` — parse a Rust type string,
  render it as TypeScript or Kotlin.
- `rpc-methods.generated.ts` gains `RpcParamsMap` / `RpcParams<M>` and
  `RpcResultMap` / `RpcResult<M>`. `call(method, params)` in `client.ts` is now
  checked on both halves, and the hand-written duplicates are gone.
- `RpcParams.generated.kt` (new) — one params builder per RPC. Lives in the
  `domain` package beside the wire types rather than in `ble` beside
  `RpcMethods.kt`, because the builders reference the payload enums and
  `domain` already depends on `ble` for `ShepherdJson`.
- Both new artifacts joined the drift test.

Results are typed as they arrive **on the wire**, so a `wrap_result` method is
`{ new_deadline: … }` rather than the value inside it. That keeps the unwrap
visible at the call site and made the change purely a typing one: no call site
changed what it returns.

## Deliberately not done

- **`Report` / `AvailabilityView`** (`shepherd-config-wasm`, mirrored by hand in
  `src/config/model/`). Generatable, but the crate needs a `schema` feature and
  a dependency edge first, and `Issue.kind` is a `&'static str` — `schemars`
  would render it `string`, *losing* the eight-literal union the hand-written
  file has. Make `kind` a real enum first, then generate. Nothing in the editor
  branches on it today, so the union is currently decorative; the field-level
  drift risk on `AvailabilityView` is the more real one.
- **`Patch` ↔ `patches.ts`.** Generatable, but it is a four-variant closed
  vocabulary already pinned by a paired fixture test.
- **`windows.ts`, `tokenSources.ts`, `reasonLabel()`, `duration.ts`.** These
  mirror *behaviour*, not types — day-mask semantics, the four self-unlock
  shapes, child-facing phrasing. There is no schema to render them from, and
  paired tests are the right guard.

## Verified

`cargo test --workspace`, `cargo clippy --workspace --all-targets -D warnings`,
`cargo fmt --check`, `npm run typecheck`, `npm test`, `npm run build`,
`check:boundary`, `check:coverage`, and — with `ANDROID_SDK_ROOT=/opt/android-sdk`
— `./gradlew :app:compileDebugKotlin` and `:app:testDebugUnitTest`.

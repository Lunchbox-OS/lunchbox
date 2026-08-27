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

## Follow-up, same day: `Report` and `AvailabilityView`

Done in a second commit, and the enum came first as planned.

`Issue::kind` was a `&'static str` assigned by hand in
`From<&ValidationError> for Issue`. Generating from that would have rendered
`string` and *lost* the eight-name union the hand-written mirror had, so it is
an `IssueKind` enum now — which also stops the `From` impl being stringly-typed.
`shepherd-config-wasm` gained a `schema` feature (off by default, so `schemars`
never reaches the browser artifact; verified with `cargo tree`), and
`editor_schema.rs` roots the five types the way `wire_schema.rs` roots the wire
ones.

`report.ts` and `availability.ts` keep their helpers — `issuesForEntry`,
`MINUTES_PER_DAY` — and re-export the types from
`model/wasm-types.generated.ts`. `Week` stays hand-written: it is a Rust type
alias rather than a struct, so `schemars` inlines it and every `Week` in the
generated view reads as `Span[][]`.

One thing fell out of this. `check-schema-coverage.mjs` excluded exactly
`config.generated.ts` from its source corpus, which would have let the new
generated file mark config fields as covered because it happens to spell
`group`, `kind`, `start` and `end` too. It now excludes `*.generated.ts` as a
class. Coverage still passes at 126/126, so nothing was relying on the looser
rule.

## Follow-up: the build stamp the docstring already promised

`versions_json()` in `lib.rs` was exported to the browser and never called.
Its doc comment said "the editor shows both so a config written for a newer
shepherd fails legibly rather than mysteriously" — half true at best. The
schema version does reach the UI, but through a different mechanism entirely
(`Report::Version`, rendered by `IssueList`, which computes `expected` from
`CURRENT_CONFIG_VERSION` itself). `crate_version` reached nothing.

That half is worth having precisely because the standalone editor is deployed
on its own subdomain and talks to no device: a stale cached bundle is
indistinguishable from a current one until it disagrees with a daemon, and then
the first question is which build was open. It is a `v0.3.7` caption beside the
title now, with the schema version in its tooltip.

`versions_json()` builds a `Versions` struct rather than an ad-hoc
`serde_json::json!` object, so it is rooted in `editor_schema.rs` with the rest
and its TypeScript comes from the same generated file — no new hand-written
mirror one commit after removing two.

## Deliberately not done

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

The follow-up also ran `./scripts/shepherd build config-editor`, which builds
the wasm through `wasm-pack` and then the standalone bundle: the artifact came
out at 851 kB, and `cargo tree -p shepherd-config-wasm` shows no `schemars`
without the feature.

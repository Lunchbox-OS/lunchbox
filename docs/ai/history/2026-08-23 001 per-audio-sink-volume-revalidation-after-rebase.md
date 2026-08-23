# Per-audio-sink volume — re-validation after the rebase (issue #124)

Prompt: "did you do the end-to-end validation too" → "Scarlett reattached, phone
is available, go ahead".

The original pass is
<docs/ai/history/2026-08-22 001 per-audio-sink-volume-e2e-validation.md>. It ran
against base `a29fc48` (#142, media-kind). The branch has since been **rebased
onto `e3c5462`** (#145, warning-channel), so that validation no longer described
the code on the branch. This note records the re-run and only calls out what
differed; everything the original note claims still holds unless said otherwise.

## Why re-run rather than trust the static gates

The rebase re-merged 16 files touched by both the branch and newly-merged main,
and they cluster on the wire boundary the feature crosses:

- `docs/rpc-schema.json`, `crates/shepherd-wire-codegen/src/wire_schema.rs`
- generated clients: `shepherd-webui/src/api/rpc-methods.generated.ts`,
  `.../types.ts`, `companion-android/.../WireTypes.generated.kt`
- `crates/shepherd-management/src/service.rs`, `crates/shepherd-api/src/events.rs`

A schema/codegen skew across the three clients compiles clean and passes every
unit test. The original pass had already found one live bug in exactly this area
(the web UI receiving no events), which is the precedent for not trusting green
gates here.

The branch's own patch was also diffed before vs. after the rebase: same file
set, and the only deltas are blob hashes, hunk offsets, and two import lists
correctly re-merged against main's new `DiagnosticSet` / `browser` additions.

## Static gates (post-rebase)

| Gate | Result |
| --- | --- |
| `cargo test --all-targets` | 838 passed, 0 failed, 17 ignored (46 suites) |
| `cargo clippy --all-targets -- -D warnings` | clean |
| `cargo fmt --all --check` | clean |
| `./scripts/shepherd config validate` | passes |
| `:app:testDebugUnitTest` | 46 passed, 0 failed (forced `--rerun-tasks`) |
| `npm run typecheck` | clean |

Gradle reported the test task `BUILD SUCCESSFUL in 1s` from cache; the run was
repeated with `--rerun-tasks` so the 46/0 is a real execution, not a replay.

## Rig

Same shape as the original: both outputs real, so every switch is unambiguous.

- Built-in analog — `alsa_card.pci-0000_00_1b.0:output:analog-output-lineout`,
  classified `line_out`.
- Focusrite Scarlett 2i2 (USB) —
  `alsa_card.usb-Focusrite_Scarlett_2i2_USB-00:output:analog-output`,
  classified `unknown`.
- Pixel 10a (Android 17) on adb, still bonded and claimed; the branch's debug APK
  installed with `adb install -r`, so the claim token survived.
- Firefox over Marionette inside the headless session for the web UI.

Both outputs were discovered automatically, with the same keys and
classifications as the original run — the generated wire types still agree
across the three clients after the rebase.

## Requirement 1 — limits per output

Every case read back two ways (RPC `get_volume` / `list_audio_outputs`, and the
hardware via `wpctl`), all passing:

- **Clamp on switch, no per-output cap.** Built-in raised to 100% behind the
  daemon's back while unselected; selecting it dropped it to the global 80.
- **Clamp on set.** Cap set to 30 while built-in was live → hardware to 30.
- **Clamp on switch, with a per-output cap.** Capped 30, raised to 100 while
  unselected, selected → 30.
- **Stricter-of-two, both directions.** Per-output 95 vs global 80 → 80;
  per-output 50 → 50; per-output floor 20 raised `set_volume 5` → 20.
- **Per-output isolation.** Built-in 30 / Scarlett 60, three alternating rounds,
  each reporting its own percent, its own ceiling, and its own identity.
- **Validation.** `max_volume=101`, `min_volume=101`, and `min > max` all
  `bad_request` with specific messages; an unknown key is `not_found`.

## Requirement 2 — the display follows the selected output

Three driven switches, each asserted in the HUD pixels: built-in → 30%,
Scarlett → 60%, built-in → 30%. The `VolumeChanged` events carry the full output
identity and that output's own restrictions.

## The web UI

Rendered and driven, not merely typechecked. The card lists both devices with
the active one badged "In use now" and the other offering "Use this", and the
header reads the active output's volume and name.

- **The event stream still works.** An output switch made behind the UI's back
  was followed live, with no reload: header name, header percent, and the row
  badges all moved. A subsequent external `set_volume` was followed the same way.
  This is the regression the original pass found and fixed; it survived the
  rebase.
- **Switching from the web UI works.** Clicking "Use this" on a row switched the
  daemon and clamped to that output's own ceiling.

## The companion, on the phone

- Reconnected on its existing bond and claim after `adb install -r`.
- The device-controls card lists both outputs with per-device limit toggles and
  sliders, and maps kinds to labels — `line_out` → "Line out", `unknown` →
  "Audio device".
- Per-output limits persisted across a daemon restart (60 / 30).
- **Switching from the phone works.** Tapping "Use this" moved the daemon and
  the hardware to the built-in output at its own ceiling.
- **Setting a limit from the phone works.** Dragging the built-in Max slider set
  the cap to 46; `set_volume 100` then landed at 46 and the hardware followed.
  The phone's own UI updated live for the switch made from it.

## Found while validating

### The dev host has two Bluetooth controllers, and the default picks the wrong one

The companion sat on "Connecting…" indefinitely. Not a code regression — the
phone was bonded to `8C:68:8B:41:02:DC` (bluetoothctl's `[default]`), while
`[service.ble_management]` left `adapter` unset and the daemon took "the first
one BlueZ lists", which resolved to the *other* radio, `DC:56:7B:1F:7D:EA`.
The phone's GATT connect attempts to `…02:DC` therefore never met a server.

This is exactly the ambiguity `config.example.toml` already warns about in the
`adapter` comment. It is a **rig** gotcha, not a product one, but it costs real
time because the symptom is a silent spinner on the phone with a healthy daemon
advertising on the other radio. Pinning `adapter = "8C:68:8B:41:02:DC"` in a
copy of the config and rebooting the session connected the peer immediately.

Which controller BlueZ lists first is not stable across boots, so any future
companion validation on this host should pin the adapter to the one the phone is
bonded to rather than assume the default.

### HUD screenshots can capture a stale frame

Twice during this pass the HUD screenshot disagreed with the daemon (25% vs 30%,
then 12% vs 46%), and twice a re-shot a moment later matched. The events had
been delivered in both cases — the daemon log shows the matching
`VolumeChanged`. `dev shot` forces `dpms on` after swayidle has blanked the
output, and the first frame back can be whatever the HUD last drew. **Re-shoot
before believing a HUD/daemon disagreement**; the skill's stale-pixel warning
applies to the idle-blank path too, not only to occlusion by a fullscreen window.

### `select_audio_output` returns `bad_request`, not `not_found`

`set_audio_output_limits` on an unknown key gives `not_found`, while
`select_audio_output` gives `bad_request` ("audio output is not connected").
This is deliberate and worth not "fixing": rows outlive the hardware, so a row
can legitimately exist while its device is absent, and asking to switch to an
absent device is a bad request rather than a missing resource.

### Driving the RPC socket from a harness

Two traps cost time here and are worth writing down for the next agent:

- **Errors are `result.err`, not a top-level `error` key.** The envelope is
  `{"request_id":…,"api_version":1,"result":{"ok":…}}` or
  `{…,"result":{"err":{"code":…,"message":…}}}`. A harness that looks for
  `error` sees every rejection as a success and reports the validation cases as
  broken when they are fine.
- **`nc -U` does not exit.** The server holds the connection open, so a
  `printf … | nc -U dev-runtime/shepherd.sock` pipeline hangs rather than
  returning the reply. Speak the socket from a real client that reads one
  newline-terminated frame and closes.

Also re-confirmed the skill's `pkill -f` warning the hard way: `pkill -f "nc -U
dev-runtime"` matched the very shell running it and killed it (exit 144). Use a
`ps -eo pid,cmd` listing and kill the pids it names.

## Still not covered

Unchanged from the original note — the hardware to exercise these is still not
present:

- Bluetooth audio: the `bluez5` branch of `kind` detection and the
  `api.bluez5.profile` route analogue.
- An output that classifies as `Headphones` or `Speakers`; both devices here are
  `line_out` and `unknown`, so those icon/label mappings remain unit-tested only.
- Latency: the 2s poll is still the bound.

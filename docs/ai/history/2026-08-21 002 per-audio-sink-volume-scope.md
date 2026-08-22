# Per-audio-sink volume — scope (issue #124)

Source: <https://git.armeafamily.com/albert/shepherd-launcher/issues/124>

Prompt: "scope out #124".

## Issue text (verbatim)

> The volume limits need to be able to be set per audio sink so that the limit
> can be set differently for headphones vs speakers.
>
> Additionally, the displayed volume needs to update when the selected audio
> sink does too.

## Requirements, restated

1. Volume **limits** (`max_volume` / `min_volume` / `allow_mute` /
   `allow_change`) must be configurable **per audio output**, so headphones can
   be capped lower than speakers.
2. The **displayed** volume (HUD, web UI, companion) must refresh when the
   selected audio output changes.

## Decisions

Settled with the maintainer on 2026-08-21.

### Config model: discovery-driven, persisted in the store

The parent never hand-writes an output matcher. shepherdd enumerates outputs,
the management UIs list what has been seen, and the parent sets a cap per row.

`config.toml` is never written by the daemon (only `shepherd-e2e` rewrites it),
so UI-set caps cannot live there. They go in the SQLite store, following the
**auto-brightness precedent** — stored value wins when present, config value is
the default when absent (`shepherdd/src/main.rs:304-311`,
`AUTO_BRIGHTNESS_SETTING_KEY`):

- New `audio_outputs` table (rows + `last_seen`, shaped after `daily_overrides`
  rather than the flat `settings` k/v, which cannot express rows).
- Key: `(device_name, route_name)` — see "Choosing the identity key".
- `[service.volume]` remains the default for any output with no stored row.

This retires the per-output config syntax and the whole matcher-vocabulary
question: no new TOML, no `[[service.volume.outputs]]`.

### Behaviour

1. **Clamp immediately on switch.** When an output becomes active and its
   remembered volume exceeds the applicable cap, shepherdd pulls it down at
   once. This is what makes the feature hearing protection rather than a
   suggestion. Accepted cost: it overrides a volume deliberately set on that
   output before the cap existed.
2. **Unseen outputs inherit the global `[service.volume]` cap.** Consistent
   with every other unconfigured thing, and predictable. A first-time headset
   is therefore not protected at headphone levels until the parent sets a row.
3. **Activity and per-output caps combine as the lower of the two.** Gaming at
   60 plus headphones at 50 yields 50. Always fails safe; a parent cannot
   deliberately *raise* a cap for one activity on one output.
4. **`kind` is advisory only** — it drives the row icon and nothing else. It
   classified neither device on the dev host, so no behaviour depends on it.

### Sequencing

**Change 1 — display refresh.** The PipeWire watcher, `VolumeChanged` carrying
full state, the HUD fix, and the active-output readout. Fixes a bug that exists
today and is independently verifiable.

**Change 2 — per-output caps.** Store table, resolution with the lower-of-two
rule, clamp-on-switch, RPC surface, and the management UI rows.

### Settled without needing a decision

- **Watcher mechanism**: poll `pw-dump` on a ~2s ticker for the first cut,
  reusing the Phase 1 parser and the auto-brightness loop's shape. Move to
  `pw-mon` only if the update latency proves visible.
- **Which UIs get the rows**: the management surfaces — webui `AdminPage` and
  the companion's `DeviceControlsScreen`. The HUD is the child-facing surface
  and gets only the active output's name/icon, not a device manager.
- **Non-PipeWire hosts**: PulseAudio and ALSA keep today's flat behaviour; the
  watcher and per-output caps are PipeWire-only.
- **Pruning**: rows carry `last_seen` and the management UI offers a delete, so
  the list does not grow without bound.

## Where volume lives today

| Layer | File | Sink awareness |
| --- | --- | --- |
| Host trait | `crates/shepherd-host-api/src/volume.rs` | none — `set_volume(percent)`, `VolumeStatus { percent, muted }` |
| Host impl | `crates/shepherd-host-linux/src/volume.rs` | none — every command targets `@DEFAULT_AUDIO_SINK@` / `@DEFAULT_SINK@` / ALSA `Master` |
| Config (raw) | `crates/shepherd-config/src/schema.rs:674` `RawVolumeConfig` | none — one flat table |
| Config (policy) | `crates/shepherd-config/src/policy.rs:737` `VolumePolicy` | none |
| Resolution | `crates/shepherd-management/src/service.rs:891` `volume_restrictions()` | entry override → global fallback; no sink dimension |
| Enforcement | `service.rs:689` `restrictions.clamp_volume(percent)` | only inside `set_volume` |
| Event | `crates/shepherd-api/src/events.rs:78` `VolumeChanged { percent, muted }` | none |
| Broadcast | `service.rs:1010` `broadcast_volume_change()` | fires **only** from shepherdd's own mutations |
| HUD | `crates/shepherd-hud/src/state.rs:250`, `app.rs:921` | caches `VolumeInfo`, updated only by that event |

The only sink-aware code in the repo is
`crates/shepherd-host-linux/src/audio_route.rs` (issue #87 dock routing). It
already parses `pw-dump` for `Audio/Sink` nodes plus the `default.audio.sink`
metadata, and drives `wpctl set-default`. That parser is the natural base for
sink enumeration here.

## Key finding: "sink" is the wrong key for the headphones-vs-speakers case

Verified against PipeWire on the dev host:

```
id 51  Device  api.alsa.path=hw:0  alsa.card_name="HDA Intel"
         Route  analog-input-linein   (Input)
         Route  analog-output-lineout (Output, device 3)
id 52  Node    node.name=alsa_output.pci-0000_00_1b.0.analog-stereo
                media.class=Audio/Sink  device.id=51
```

There is **one** `Audio/Sink` node and a set of output **Routes** (ports) on the
parent `Audio/Device`. On a laptop, plugging headphones into the analog jack
switches the active route (`analog-output-lineout` → `analog-output-headphones`)
and leaves `node.name` completely unchanged. Matching a limit on sink node name
alone therefore **silently fails for the exact use case the issue names**.

USB headsets and Bluetooth headphones *do* appear as distinct sink nodes, so
both dimensions are needed. The identity key must be
**(sink node, active output route)**.

## Key finding: the stale display has the same root cause

WirePlumber persists volume **per card:route**. From this host's
`~/.local/state/wireplumber/default-routes`:

```
alsa_card.pci-0000_00_1b.0:output:analog-output-lineout={"mute":false, "channelVolumes":[1.000000, 1.000000], ...}
```

So the real hardware volume genuinely changes when the route or default sink
changes — the HUD is not merely failing to redraw, it is showing a number that
no longer describes the hardware.

Nothing in shepherdd observes PipeWire. `VolumeChanged` is broadcast only from
`broadcast_volume_change()`, which is reached only from `set_volume`,
`set_mute`, `volume_up`, `volume_down`, `toggle_mute`. A sink switch (jack
insert, Bluetooth connect, dock via `audio_route.rs`, or a bare `wpctl` call)
produces no event at all.

## Key finding: restrictions go stale too, and the HUD is the only client that splices

- Web UI: `useEvents.ts` invalidates *all* queries on any SSE event, then
  refetches `getVolume()` — gets a fresh, complete `VolumeInfo`.
- Companion: `ShepherdViewModel.kt:460` maps `VolumeChanged` → `refreshVolume()`
  → `getVolume()` — likewise complete.
- HUD: `HudState::update_volume` (`state.rs:250`) writes only `percent` and
  `muted` into the cached `VolumeInfo` and **deliberately preserves the
  restrictions from the initial fetch**.

Today that is cosmetic. With per-output limits it becomes a correctness bug: the
HUD slider would keep enforcing the speaker cap while headphones are plugged in.

## Key finding: limits are never re-applied, only applied on change

`clamp_volume` runs only inside `set_volume`. If the switch lands on an output
whose remembered volume already exceeds the new cap (headphones remembered at
100%, cap 60%), nothing pulls it down. For a hearing-protection feature the
clamp has to be applied **at switch time**, which needs a host-side apply path,
not just a clamp in the request handler.

## Choosing the identity key

Verified against the live PipeWire on the dev host (`pw-dump`, WirePlumber
scripts in `/usr/share/wireplumber/`, ACP port configs in
`/usr/share/alsa-card-profile/`).

### Tier 1 — stable across reboots and replugs

**`device.name` + route `direction` + route `name`.** This is the key
WirePlumber itself uses to persist per-output volume
(`scripts/device/state-routes.lua:288`):

```lua
local key = dev_info.name .. ":" .. route.direction:lower () .. ":" .. route.name
```

`dev_info.name` is `device_properties["device.name"]`
(`scripts/lib/device-info-cache.lua:44`), yielding the key observed in this
host's state file:

```
alsa_card.pci-0000_00_1b.0:output:analog-output-lineout={"channelVolumes":[1.0,1.0],...}
```

Keying shepherd's limits identically means the limit and the volume PipeWire
remembers can never disagree about what "an output" is. `device.name` is
profile-independent (unlike `node.name`); WirePlumber derives it from
`device.name` / `device.bus-id` / `device.bus-path` with a `.2`/`.3` dedup
suffix for identical cards (`scripts/monitors/alsa.lua:411`).

Route names come from a **closed, on-disk vocabulary** — 28 output port configs
in `/usr/share/alsa-card-profile/mixer/paths/`: `analog-output-headphones`,
`analog-output-speaker`, `analog-output-lineout`, `hdmi-output-N`,
`iec958-stereo-output`, plus device-specific ones
(`usb-gaming-headset-output-stereo`, `steelseries-arctis-output-game-common`).

**`api.bluez5.address`** (MAC) is the equivalent for Bluetooth — survives
reboots, and is embedded in the generated `node.name`/`device.name` anyway.

### Tier 2 — stable while physical topology is fixed

- `node.name` (`alsa_output.pci-0000_00_1b.0.analog-stereo`) — embeds the
  profile, so it changes with the profile, and cannot distinguish headphones
  from speakers at all.
- `device.bus-path` / `device.sysfs.path` — break on a PCI-slot or USB-port
  change. Note these are usually *not* what `device.name` is built from for USB
  (see below), so a USB device's key is more stable than its bus-path suggests.
- `device.vendor.id` / `device.product.id` — per model, not per unit.

### Tier 3 — semantic classification, not identity

`device.form-factor`, with the vocabulary confirmed from WirePlumber's icon maps
(`monitors/alsa.lua:467`, `monitors/bluez.lua:380`): `internal`, `speaker`,
`headphone`, `headset`, `hands-free`, `tv`. Works for Bluetooth and USB
headsets; **useless for the analog jack** — on this host it reads `internal`,
which is true of the card and says nothing about the live port. Same for
`device.bus` and `device.class`.

### Tier 4 — never use for identity

- **Node / object id** (`52`) — recycled across daemon restarts and replugs. A
  `wpctl` argument, nothing more.
- **`object.serial`** — unique and never reused, but only within one daemon
  lifetime. Fine for a watcher's change detection, never for config.
- **`api.alsa.path`** (`front:0`, `hw:0`) — ALSA card index, shifts with
  enumeration order. Acceptable as the substring *hint* `looks_external_video`
  already uses it for; not as a key.
- **`node.description` / `device.description` / nicks** — localized via
  `I18n.gettext` and mutable. Display only.

### Verified against two simultaneous devices

A USB interface (Focusrite Scarlett 2i2) was passed through to the dev VM
alongside the built-in analog card, giving a real two-output host:

```
  Built-in Audio Analog Stereo
      key      alsa_card.pci-0000_00_1b.0 : output : analog-output-lineout
      kind     Unknown   (form-factor=internal bus=pci)
      jack     available=unknown -> usable=True      default  False

* Focusrite Scarlett 2i2 Analog Stereo
      key      alsa_card.usb-Focusrite_Scarlett_2i2_USB-00 : output : analog-output
      kind     Unknown   (form-factor=None bus=usb)
      jack     available=unknown -> usable=True      default  True
```

**"Available" is three separate questions.** All three are needed:

1. *Enumerated* — the device appears in `pw-dump`.
2. *Jack availability* — the route's `available` field: `yes` / `no` /
   `unknown`. Both cards here report `unknown` because neither does jack
   detection, so the rule must be **`usable = (available != "no")`**. A
   `available == "yes"` test would hide every output on this host.
3. *Selected* — `default.audio.sink`.

**Semantic `kind` classification fails on both of these devices.** The
Focusrite's route is the generic `analog-output` (not `-speaker` / `-headphones`)
and `device.form-factor` is **absent entirely**; the built-in reports
`internal`, which describes the card and not the live port. Both classify as
`Unknown`.

Consequently a config written purely as `match = "headphones"` matches nothing
on this host. The exact `(device.name, route)` matcher is therefore **not an
escape hatch** — for line-outs and generic USB interfaces it is the only thing
that works, and it must be a first-class config path. Semantic kinds are a
convenience layer for devices that self-describe (Bluetooth headsets, laptop
jacks with real port names).

**`device.name` is more stable for USB than the bus-path suggests.**
WirePlumber builds it from `device.name or device.bus-id or device.bus-path`
(`monitors/alsa.lua:411`). The Focusrite has
`device.bus-id = usb-Focusrite_Scarlett_2i2_USB-00`, so its key ignores
`device.bus-path = pci-0000:02:00.0-usb-0:3:1.0` and survives a move to a
different USB port. The PCI card has no bus-id, falls through to bus-path, and
is slot-dependent. Edge case: two identical USB devices with no unique serial
collide and receive WirePlumber's `.2` dedup suffix, which *is*
enumeration-order dependent.

**The hotplug reproduced the issue's second half live.** No `default-nodes`
state file exists and nothing was chosen by hand — WirePlumber moved
`default.audio.sink` to the Focusrite purely on `priority.session` (1109 vs
1009). Plugging in any USB audio device silently changes which output is
selected, and under per-output limits would silently change which limit
applies. The watcher must therefore react to device **add/remove**, not just
route changes, and this is further weight behind enforcing the clamp on switch.

**`default-routes` is a cache, not an enumeration source.** It still contains no
entry for the Focusrite: WirePlumber writes per-route volume lazily, on first
change. Enumeration must come from `pw-dump`; the state file is only evidence
of the keying scheme.

A prototype enumerator implementing the above (identity key, kind derivation,
usable/default flags) is the basis for Phase 1.

### Consequence for this issue

The matcher is a two-part key mirroring WirePlumber's — `device.name` plus route
name. Exact `(device.name, route)` matching is the load-bearing path: it is the
only mechanism that addresses a generic USB interface or a line-out. On top of
it, a semantic `kind` (derived from route name first, then `device.form-factor`,
then `device.bus`/`device.api`) is a convenience for self-describing devices —
route name covers the analog headphone jack the issue names, form-factor covers
Bluetooth and USB headsets, and neither covers the two devices actually present
on the dev host. Any output that classifies as `Unknown` must fall back to the
flat `[service.volume]` limit.

### Adjacent bug found while checking this

`PipeWireAudioRouter` saves `saved_default: Mutex<Option<u32>>` — a raw node id
— and replays it via `wpctl set-default <id>` on undock
(`audio_route.rs:151,220`). Across a PipeWire restart or a device
re-enumeration between dock and undock that id is stale, or has been recycled
onto a different node. It should save `node.name` and re-resolve the id at
restore time. Independent of #124, but the same identity mistake.

## How `kind` detection would work

Three signals, in strict priority order. Researched against the shipped ACP port
vocabulary, `/usr/lib/udev/rules.d/78-sound-card.rules`, and WirePlumber's
bluez monitor.

### Signal 1 — active route name (the only port-level signal)

Closed vocabulary: 27 output port configs in
`/usr/share/alsa-card-profile/mixer/paths/`. Normalize by lowercasing and
stripping a trailing `-<digits>` (`analog-output-headphones-2`, `hdmi-output-7`),
then substring-match. Exercised over the complete vocabulary, **22 of 27
classify**:

| stem contains | kind |
| --- | --- |
| `headphone`, `headset`, `chat` | Headphones |
| `speaker` (incl. `-speaker-always`) | Speakers |
| `hdmi`, `displayport` | Hdmi |
| `iec958`, `spdif` | Digital |
| `lineout` | LineOut |

The 5 that carry no signal are `analog-output`, `analog-output-mono`,
`audigy-analog-output`, `audigy-analog-output-mirror`, and
`steelseries-arctis-output-game-common` — generic single-output cards, which is
exactly the Focusrite case on the dev host. Vendor-specific port families like
the Arctis one need explicit entries or they read as no-info; that open-ended
tail is itself an argument against leaning on `kind`.

This is the **only** signal that can distinguish headphones from speakers on a
single card, because it is the only per-port one.

### Signal 2 — `device.form-factor` (a udev name heuristic, and card-level)

`device.form-factor` is **not** authoritative hardware metadata. It is set by
`78-sound-card.rules`, which substring-matches the product model name:

```
# Matching on the model strings is a bit ugly, I admit
ENV{ID_MODEL}=="*[Hh]eadphone*", ENV{SOUND_FORM_FACTOR}="headphone"
ENV{ID_MODEL}=="*[Hh]eadset*",   ENV{SOUND_FORM_FACTOR}="headset"
ENV{ID_MODEL}=="*[Ss]peaker*",   ENV{SOUND_FORM_FACTOR}="speaker"
```

plus `internal` for anything on PCI bus `0000:00:??.?` or a platform device, and
`webcam` for a USB device that also exposes a video interface. Otherwise it is
**unset**. (The comment quoted above is the rules file's own.)

Consuming it is still worthwhile — free, hwdb-backed, maintained upstream — but
it is a hint of the *same class* as doing our own name matching, not a tier
above it. It explains both dev-host results exactly: the built-in sits on
`0000:00:1b.0` so it gets `internal`; "Focusrite Scarlett 2i2" contains none of
the magic words so it gets nothing.

It is also a property of the **card**, not the port. On a laptop the card is
always `internal`, so form-factor can never distinguish headphones from speakers
there. It only helps where the card *is* the endpoint: a USB or Bluetooth
headset.

### Signal 3 — transport (`device.api` / `device.bus`)

`device.api == "bluez5"` → Bluetooth. For BT the route analog is
`api.bluez5.profile` (`a2dp-sink` vs `headset-head-unit`), since BT devices have
profiles rather than ALSA routes, and node names are `bluez_output.<MAC>.<n>`
(`monitors/bluez.lua:95`). BT is also the one place form-factor is generally
populated properly, because the bluez5 SPA plugin derives it from the
Class-of-Device — **unverified here**, as the dev host has no BT audio device.

`device.bus == "usb"` carries no semantic meaning on its own; the Focusrite
proves it.

### The algorithm

First match wins, and `Unknown` is a legitimate outcome:

```rust
fn kind(dev: &DeviceProps, route: Option<&Route>) -> Kind {
    // 1. Port-level signal always wins.
    if let Some(k) = route.and_then(|r| route_kind(&r.name)) { return k; }
    // 2. Transport.
    if dev.api.as_deref() == Some("bluez5") { return Kind::Bluetooth; }
    // 3. Card-level, and only when the card is plausibly the endpoint.
    match dev.form_factor.as_deref() {
        Some("headphone") | Some("headset") | Some("hands-free") => Kind::Headphones,
        Some("speaker")                                          => Kind::Speakers,
        Some("tv")                                               => Kind::Hdmi,
        // `internal` is NOT a classification — it means "built-in card",
        // leaving the port question unanswered.
        _ => Kind::Unknown,
    }
}
```

Both outputs on the dev host return `Unknown`.

### Design consequence: discovery-driven, not predictive

Because `kind` yields nothing for either real device here, it cannot be the
primary mechanism — it is a display and defaulting convenience. That argues for
inverting the config model:

- shepherdd enumerates outputs and reports
  `(key, description, kind, usable, is_default)` — the Phase 1 prototype already
  produces exactly this.
- The admin UI lists outputs it has seen (persisting them so they stay settable
  while unplugged) and the parent sets a cap per row.
- Config stores exact `(device.name, route)` keys. `kind` drives the row icon
  and an optional default (`if kind == Headphones and no explicit rule, apply
  headphone_max`).

The parent has the headphones in hand: plug in, see the row appear, set the cap.
Nobody has to predict whether udev will populate `ID_MODEL` for their headset.

## Proposed work breakdown

### Phase 1 — host: output identity

Promote the `pw-dump` parsing out of `audio_route.rs` into a shared
`shepherd-host-linux::audio` module and expose the currently-selected output:

```rust
pub struct AudioOutput {
    /// Stable half of the key: `device.name`, e.g. `alsa_card.pci-0000_00_1b.0`.
    pub device_name: String,
    /// Discriminating half: active output route, e.g. `analog-output-headphones`.
    pub route_name: Option<String>,
    /// Node the volume commands actually land on; re-resolved, never cached as an id.
    pub node_name: String,
    /// Display only — localized and mutable.
    pub description: String,
    pub kind: AudioOutputKind,   // Speakers | Headphones | Hdmi | Bluetooth | Other
}
```

`(device_name, route_name)` is the identity key, chosen to match WirePlumber's
own persistence key — see "Choosing the identity key" above.

`kind` is derived from route name (`*-headphones`, `*-headset`), node name /
`api.alsa.path` (`hdmi`, `displayport` — the existing `looks_external_video`
heuristic), and `device.api` (`bluez5`).

Keep `VolumeController` operating on the default sink — every existing call
stays correct — and add `fn current_output(&self) -> Option<AudioOutput>`.
Non-PipeWire backends return `None`.

### Phase 2 — config: per-output limits

```toml
[service.volume]            # unchanged: the fallback for any unmatched output
max_volume = 80

[[service.volume.outputs]]
match = "headphones"        # closed enum, precedent: RawInputDevice
max_volume = 50

[[service.volume.outputs]]
match = "bluetooth"
max_volume = 60
```

Recommend the closed-enum matcher first (it validates, and the config is written
by a parent, not by someone reading `pw-dump`), with an optional `name =
"<substring>"` escape hatch for specific hardware. Entry-level
`[entries.volume]` keeps working; the layering needs a decision (see open
questions).

`validation.rs` currently validates **nothing** about volume — add 0-100 range
and `min <= max` checks for both the flat and per-output tables while touching
this. Update `config.example.toml` (and re-run config validation, per
`CLAUDE.md`).

### Phase 3 — daemon: observe and react

A watcher task in `shepherdd`, shaped like the existing auto-brightness poll
loop (`crates/shepherdd/src/main.rs:355`). On a default-sink or active-route
change: re-resolve restrictions for the new output, read the actual volume,
clamp it if it exceeds the new max, and broadcast.

Mechanism choice: `pw-mon` subprocess (event-driven, no idle cost, needs stream
parsing) vs. polling `pw-dump` on a ~2s ticker (trivially simple, reuses the
Phase 1 parser). Polling is the smaller first cut.

This also picks up externally-made volume changes, which fixes the same
staleness class for free.

### Phase 4 — API and event surface

- `VolumeInfo` grows `output: Option<AudioOutputInfo>`.
- Recommend making `VolumeChanged` carry the full state (percent, muted,
  restrictions, output) rather than adding a parallel event — that removes the
  HUD's partial-splice path by construction and fixes the stale-restrictions
  bug.
- Regenerate the wire mirrors:
  `cargo run -p shepherd-wire-codegen --bin rpc-codegen` →
  `docs/rpc-schema.json`, `RpcMethods.kt`, `WireTypes.generated.kt`,
  `rpc-methods.generated.ts`. Check `WireTest.kt` for the
  older-companion-vs-newer-daemon compatibility expectation before adding
  non-nullable fields.

### Phase 5 — clients

- **HUD**: consume the full payload; icon, label, and slider bounds all follow
  the new restrictions. Optionally show the active output's name.
- **Web UI** (`AdminPage.tsx`) and **companion** (`DeviceControlsScreen.kt`):
  both already read `restrictions.min/max` and both already refetch on the
  event, so they mostly come along for free — worth surfacing the active output
  name so a parent can tell which limit is in effect.

### Verification

- Unit: output classification from recorded `pw-dump` JSON (the existing
  `audio_route.rs` tests are the template); per-output restriction resolution
  and layering; config validation.
- Integration: `crates/shepherdd/tests/integration.rs` — sink change produces a
  `VolumeChanged` carrying the new restrictions.
- End-to-end: `headless-dev` skill for the HUD readout. Note the headless dev
  host has a single analog sink, so the headphones case needs either a real
  laptop jack, a Bluetooth headset, or a synthetic PipeWire null-sink + a fake
  `pw-dump` fixture.

## Change 1 as built (display refresh)

Landed 2026-08-21. Per-output caps (change 2) are not part of this.

### Shape

- `crates/shepherd-host-linux/src/audio.rs` (new) — the `pw-dump` parser, output
  enumeration, identity keys, and `kind` classification. `audio_route.rs` now
  reuses it instead of carrying its own copy.
- `shepherd-api` — `AudioOutput { key, description, kind }` and
  `AudioOutputKind`; `VolumeInfo.output`; `VolumeChanged` extended with
  `restrictions` and `output`.
- `shepherd-host-api` — `VolumeController::current_output()` and
  `observe()`, both defaulted so non-PipeWire backends are untouched.
- `shepherd-management` — `audio_watch_tick()` plus `ObservedAudioState`;
  `broadcast_volume_change` now sends the whole snapshot.
- `shepherdd` — the 2s watch loop, started only on PipeWire hosts.
- `shepherd-hud` — `update_volume` replaces instead of merging; the active
  output's name goes in the mute button's tooltip.
- `shepherd-webui` — `AudioOutput` types and an "Output:" caption on the volume
  card.
- Wire mirrors regenerated with `rpc-codegen`.

### Atomicity: found by testing, not by review

The watcher first read the status and the output separately
(`get_status()` then `current_output()` — a `wpctl` spawn and a `pw-dump`
spawn). During end-to-end testing the HUD was caught once showing the previous
output's volume against the new output's identity: a switch had landed between
the two reads, and the tick published the torn pair. It self-corrected on the
next tick, so it was a sub-2-second glitch — but it is precisely the symptom
this change exists to remove.

Fixed by reading both from one dump. PipeWire exposes the sink's volume in the
same `pw-dump` as its identity, under the node's `Props` param, so
`observe()` returns a pair that cannot describe two different moments — and
costs one process spawn instead of two. The trait default keeps the old
two-read behaviour for backends that cannot do better.

Volume there is stored cubed: `channelVolumes` of `0.015625` is 25% and
`0.063997` is 40%, matching what `wpctl get-volume` prints
(`cbrt`). Both values were read off the live host and are pinned in
`cubic_volume_matches_what_wpctl_reports`.

### Verified

Unit and integration: 667 workspace tests pass; `cargo clippy --all-targets -D
warnings` clean. New coverage includes the watcher's baseline/silence/change
behaviour, the restrictions-in-the-event regression, the ACP route vocabulary,
and the cubic volume conversion.

End-to-end on the headless dev session against two real devices (built-in analog
card and a passed-through Focusrite Scarlett 2i2), with the two outputs held at
deliberately different volumes so a switch is unambiguous:

- Four alternating `wpctl set-default` switches produced four `VolumeChanged`
  events, each carrying the correct percent, restrictions, and output
  description.
- HUD screenshots after each switch read 40%, 25%, 40%, 25% with the slider
  tracking — i.e. the display now follows the selected output.
- A live test against the machine's real PipeWire
  (`cargo test -p shepherd-host-linux -- --ignored live_pw_dump`) confirms the
  Rust enumerator agrees with the topology by hand.

The web UI change is typecheck- and build-verified only; there is no browser in
the headless session.

### Known limits

- Latency is up to the 2s poll interval. `pw-mon` would make it event-driven.
- `kind` returns `Unknown` for both dev-host devices, as expected — nothing
  depends on it.
- PulseAudio and ALSA hosts get no output identity and no watch loop.

## Change 2 as built (per-output limits)

Landed 2026-08-21, on top of change 1.

### Shape

- `shepherd-store` — new `audio_outputs` table (`output_key` PK, description,
  kind, `max_volume`, `min_volume`, `last_seen`) plus five trait methods.
  `record_audio_output_seen` never writes the limit columns, so a device that
  disappears and comes back keeps its cap.
- `shepherd-api` — `AudioOutputRecord { output, max_volume, min_volume,
  last_seen, active }`.
- `shepherd-management` — `volume_restrictions_for(output_key)` layering,
  `enforce_volume_ceiling()`, discovery inside the watch tick, and three RPC
  methods: `list_audio_outputs`, `set_audio_output_limits`,
  `forget_audio_output`.
- `shepherd-webui` — a "Volume limits per device" card listing the rows, with a
  per-row on/off switch, a max slider, an "In use now" chip, a last-used label,
  and a Forget button (disabled for the active device, which would be
  rediscovered immediately).
- `config.example.toml` — a comment pointing at the admin UI, since there is
  deliberately no TOML for this.

### How the decisions came out in code

**Discovery-driven, no new TOML.** `config.toml` is never written by the daemon,
so UI-set caps live in the store, following the auto-brightness precedent
(stored value wins, config is the default). The parent plugs the device in, sees
the row, sets the cap.

**Stricter-of-two layering.** `stricter_max` takes the lower of the policy and
per-output ceilings; `stricter_min` takes the higher of the floors; a floor
above the ceiling is clamped down to it. A per-output limit can therefore lower
a cap but never raise one — pinned by
`the_stricter_of_the_policy_and_output_caps_wins`, which checks both directions.

**Clamp on switch, and on set.** `enforce_volume_ceiling` runs when the active
output changes and when a cap is saved. The second case matters as much as the
first: without it a parent sets a limit, hears nothing change, and concludes it
did not work.

**Unseen outputs inherit the global cap**, so a brand-new device is no louder
than the machine's default and no quieter.

### Verified

685 workspace tests pass; `cargo clippy --all-targets -D warnings` clean;
`config validate` passes. New coverage: seven store tests (limits survive
replug, unknown keys refused, unrecognised kinds decay, ordering, clearing) and
eleven service tests (discovery, per-output isolation, both directions of the
stricter-of rule, inheritance, clamp on switch, clamp on set, no clamp when
already quiet, survival across replug, forget, and validation).

End-to-end on the headless session with two real devices:

- Both were discovered automatically by being used; no log-digging.
- Capping the built-in at 30%, leaving it at 100% while *unselected*, then
  selecting it: volume dropped to 30% on arrival.
- The Focusrite, uncapped per-output, still reported the global 80% and clamped
  a 95% request to 80; the same request on the capped device clamped to 30.
- The HUD slider's range followed the active output — pinned at maximum showing
  30%.

### Companion rows

Added alongside the web UI, at the maintainer's request, to be validated in an
environment that can build Android:

- `ManagementClient` — `listAudioOutputs`, `setAudioOutputLimits`,
  `forgetAudioOutput`.
- `ShepherdViewModel` — `audioOutputs` in `DeviceUiState`, fetched on connect
  and refreshed on `VolumeChanged` (which can mean the active output moved, or
  that a device the list has never seen just appeared), plus the two actions.
  Both actions refetch the volume as well as the list, because the device may
  have turned the volume down to obey a newly set cap.
- `ui/device/AudioOutputsCard.kt` — the rows, mirroring the web UI: switch,
  max slider, "In use now" chip, Forget (hidden for the active device, which
  would be rediscovered immediately). Gated on `volume.available` the same way
  `VolumeCard` is, so a device with no sound backend shows nothing rather than
  an empty state.
- `WireTest.kt` — four decode assertions, including that a payload with no
  `output` field still decodes (an older device against a newer phone).

Verified with the toolchain from `./scripts/shepherd deps install android`
(JDK 21 + SDK in `/opt/android-sdk`): `:app:compileDebugKotlin` clean, all 33
`:app:testDebugUnitTest` tests pass including the three new ones, and
`:app:assembleDebug` produces an APK. `testDebugUnitTest` is exactly what CI
runs for this module, so the companion is verified to the project's own bar.

`AudioOutputRecord` had to be rooted explicitly in
`shepherd-wire-codegen`'s `WireTypes`: it is reachable only through
`list_audio_outputs`, not from `VolumeInfo`, so the Kotlin type was otherwise
never emitted.

### Not verified

- **The web UI card is not visually verified.** It typechecks and builds, and
  the payload it renders was confirmed live over HTTP, but the SPA is embedded
  into the binary at compile time and no working browser is available in the
  headless session, so nothing rendered it. The project has no JS test harness
  (no test script, no jsdom/testing-library), and standing one up is a larger
  decision than this issue should make.
- **The companion's on-device behaviour.** The Kotlin now compiles, its unit
  tests pass, and the debug APK assembles (see below), but nothing has rendered
  the card on a phone or driven it against a real device. Pairing and the BLE
  path stay a manual smoke test — see the `companion-pairing` skill.

## Open questions

All blocking questions were resolved above. Remaining items are implementation
details to settle in review, not decisions:

- Compiling and exercising the companion rows on a real device.
- Visual verification of the web UI card, which needs either a browser in the
  headless session or a JS component-test harness.
- Whether the 2s poll should become a `pw-mon` subscription. Only latency is at
  stake; correctness does not depend on it.
- `min_volume` is stored and layered per output but not exposed in the UI, which
  offers only a maximum. That is the limit the issue asked for; the floor can be
  surfaced if a use case appears.

# Per-audio-sink volume — end-to-end validation (issue #124)

Prompt: "an implementation of #124 is checked out. do the end-to-end validation,
including of the companion app. I gave you a secondary audio device as well to
test detection."

Scope and both change write-ups:
<docs/ai/history/2026-08-21 002 per-audio-sink-volume-scope.md>. This note only
covers the validation pass and what it found — it closes the three items that
document listed under "Not verified" and "Open questions".

## Rig

- Headless dev session (`shepherd dev headless`, `config.example.toml`).
- Two **real** outputs, so every switch is unambiguous:
  - built-in analog card — `alsa_card.pci-0000_00_1b.0:output:analog-output-lineout`,
    classified `line_out`;
  - a passed-through **Focusrite Scarlett 2i2** (USB) —
    `alsa_card.usb-Focusrite_Scarlett_2i2_USB-00:output:analog-output`,
    classified `unknown`, as the scope predicted for a generic USB interface.
- Pixel 10a on adb, already bonded and claimed, running the debug APK built from
  this branch (`adb install -r` — no uninstall, so the claim token survived).
- Firefox 153 driven over **Marionette** inside the headless session (see
  "Verifying the web UI headlessly" below) — the first time the web UI has been
  rendered rather than merely typechecked.

## Static gates

| Gate | Result |
| --- | --- |
| `cargo test --all-targets` | 658 passed, 0 failed, 15 ignored (41 suites) |
| `cargo clippy --all-targets -- -D warnings` | clean |
| `cargo fmt --all --check` | clean |
| `./scripts/shepherd config validate` | passes |
| `:app:testDebugUnitTest` | 40 passed, 0 failed |
| `:app:assembleDebug` | APK produced |
| `npx tsc --noEmit` (webui) | clean |

## Requirement 1 — limits per output

Every case below was read back three ways: the RPC (`get_volume`,
`list_audio_outputs`), the hardware (`wpctl get-volume`), and the UI.

- **Discovery is automatic.** Both outputs appeared as rows purely by being
  selected; nothing was hand-written and no log was dug through.
- **Clamp on switch, with no cap set.** Built-in sitting at 100% while
  unselected; selecting it dropped it to the global 80 (`Volume above the limit
  for this output; turning it down from=100 to=80`).
- **Clamp on set.** Built-in active at 80%, cap set to 30 → hardware went to 30%
  within the same call.
- **Clamp on switch, with a per-output cap.** Built-in capped at 30 and raised
  to 100% behind the daemon's back while *unselected*; selecting it dropped it
  to 30%.
- **Stricter-of-two, both directions.** Global cap 80. A per-output cap of 95
  left the effective ceiling at 80 (`set_volume 95` → 80); a per-output cap of
  50 lowered it (`set_volume 95` → 50). A per-output floor of 20 raised the
  floor (`set_volume 5` → 20).
- **Per-output isolation.** With built-in at 30 and Focusrite at 60, alternating
  switches produced 30/30 and 60/60 (percent/ceiling) every time.
- **Validation.** `max_volume=101` → `bad_request`; `min_volume=20,
  max_volume=10` → `bad_request`; an unknown key → `not_found`.
- **A real unplug/replug.** The Focusrite was physically removed and restored
  through `/sys/bus/usb/devices/1-3/authorized`. On unplug WirePlumber fell back
  to the built-in, shepherdd followed and pulled it 100% → 30% (its cap); the
  absent Focusrite kept its row and its 60 cap; on replug the switch went back
  and 60 was re-applied. **This is the closest thing on this host to the
  headphones-in/headphones-out case the issue names, and it works.**
- **Forget.** Removing a row and then re-selecting that device rediscovered it
  with no cap (the global limit applying) — `record_audio_output_seen` not
  touching the limit columns is what makes the replug case above work and the
  forget case not resurrect a stale cap.

## Requirement 2 — the display follows the selected output

- SSE carried a `VolumeChanged` for **every** switch, each with the correct
  percent, restrictions **and** output identity.
- HUD screenshots tracked the active output: 30% with the slider pinned at
  maximum (cap 30), then 40% at mid-scale (cap 80), then 60% pinned (cap 60).
- The **companion** refetched on the event with no interaction. The **web UI did
  not** — it received no events at all on a claimed device, for a reason that
  predates this branch; found here, fixed, and re-verified (see "The web UI
  received no events" below). Requirement 2 now holds in all three clients.
- **The companion followed a sink switch made on the host over BLE**: with the
  phone sitting on Device controls and untouched, `wpctl set-default` moved the
  reading 60% → 30%, moved the "In use now" chip to the other row, reordered the
  rows, and moved the "Forget this device" button to the newly-inactive row.

## The web UI card, rendered

Both rows render with icon, description, "In use now" chip, last-used label,
switch, max slider and Forget (disabled on the active row). Driven from the
browser:

- switch off → cap cleared, effective ceiling back to the global 80; switch on →
  cap restored;
- the max slider commits on release (`30 → 31 → 30` via real key events; the
  `busy` gate swallows presses during the in-flight mutation, which is why five
  presses move it by one);
- Forget removed the inactive row from the daemon and the UI.

## The companion, on the phone

Rows render with the kind label ("Line out", "Audio device"), the chip, the
switch, the slider and "Forget this device". Driven by touch:

- dragging the built-in slider set its cap to 62, then to 25 — and because the
  built-in was live at 30%, the 25 cap **pulled the hardware down to 25% and the
  phone's own Volume card followed**;
- the switch cleared and restored the cap;
- Forget removed the Focusrite row, and re-selecting that device on the host
  brought it back as "No limit" live on the phone;
- all of it survived a `force-stop` + relaunch reconnect.

## Found and fixed

Everything below is client-side, in `shepherd-webui` and `companion-android`.
**No Rust changed at any point.**

**The web UI received no events at all on a claimed device** — the larger of the
two, written up in full below.

**The per-device cap behaved differently in the browser and on the phone** in
five ways — see "The two UIs are not the same control" below.

**The web UI's per-device switch shipped with no accessible name.**
`AudioOutputsCard.tsx` passed `inputProps={{ "aria-label": ... }}`; MUI removed
`inputProps` in v7 and this project is on **v9**, so the prop was silently
dropped. Confirmed against the live DOM: the sliders (which use `aria-label`
directly) had their labels, both checkboxes had `aria-label=null`. Fixed by
moving it to `slotProps={{ input: { "aria-label": ... } }}` and re-verified in
the browser — all four inputs now carry a per-device label. It was the only
`inputProps` in the codebase.

## Found, not fixed

- **A clamping switch broadcasts `VolumeChanged` twice.** `audio_watch_tick`
  stores the *pre*-clamp reading in `last_audio_state` and then broadcasts the
  *post*-clamp one, so the next tick sees its own correction as a fresh change
  and emits an identical second event ~2s later. Observed on every clamping
  switch and on clamp-on-set; a switch that needs no clamp emits exactly one.
  Harmless — every client refetches — but it is one wasted round trip per
  switch, and the recorded baseline briefly describes a state that no longer
  exists. Re-reading after `enforce_volume_ceiling` (or storing the enforced
  value) would settle it.
- ~~**The two management UIs disagree about the per-device cap.**~~ All four
  ways are now fixed, plus a fifth (Compose rows had no stable identity) found
  while fixing them. See "The two UIs are not the same control" below.
- **`min_volume` is layered but never enforced on switch.**
  `enforce_volume_ceiling` only lowers. An output arriving below its floor stays
  there. Neither UI exposes a floor today, so nothing reaches this; worth
  knowing if one ever does.
- **The companion's switch and sliders are `NAF` in the accessibility tree** (no
  content description). Pre-existing house style — the long-standing Volume
  slider and Muted switch are the same — so not a regression from this change,
  but the per-device controls are the first place where the *row* is what
  distinguishes one control from another.

## The web UI received no events — found, fixed, re-verified

Chased down while looking into the UI divergence below, and it is the more
serious of the two findings.

`sseUrl()` (`shepherd-webui/src/api/client.ts`) appends the token as a **query
parameter**, because the native `EventSource` constructor cannot set request
headers:

```ts
return token ? `${url}?token=${encodeURIComponent(token)}` : url;
```

`require_auth` (`crates/shepherd-http/src/auth.rs`) reads **only** the
`Authorization` header and never looks at the query string. Measured against the
running daemon:

| Request | Status |
| --- | --- |
| `GET /api/v1/events` with `Authorization: Bearer <tok>` | 200 |
| `GET /api/v1/events?token=<tok>` | **401** |
| `GET /api/v1/events` | 401 |

So the browser's `EventSource` 401s, errors, and closes (`readyState=2`,
confirmed from the page); `useEvents` reconnects every 5s and fails the same way
forever. Instrumenting the live page with its own `EventSource` recorded **zero**
events across an external change.

**Both halves are on `main`** — this is not a regression from #124. It has gone
unnoticed because the gate is open when no static `auth_token` is set *and* no
admin has claimed the device (`AuthSources::is_open`), which is every unclaimed
dev box. **Live updates in the web UI break exactly when the device is claimed**,
i.e. on every real deployment. The companion is unaffected (BLE, not SSE) and so
is the HUD (IPC socket) — the web UI is the only client that reads events this
way.

Two consequences observed directly, both on the #124 card:

- **It does not follow a sink switch.** `wpctl set-default` moved the active
  output to the built-in; the daemon agreed, the phone followed, and the web UI
  kept "In use now" on the Focusrite indefinitely. That is requirement 2 of this
  issue failing in one of the three clients.
- **It silently overwrites a cap set from the phone.** With the daemon holding
  42 (set over BLE) and the browser still rendering a stale `Max 90%`, one
  off/on of the web UI's switch wrote **90** back. The parent sets a limit on
  their phone, walks to the TV, touches an unrelated control, and their setting
  is gone.

### The fix is client-side, and the Bearer token works fine

The first instinct is to teach `require_auth` to read `?token=`, which puts a
credential in URLs and therefore in logs and history. **That is not necessary.**
The limitation belongs to the `EventSource` *constructor*, not to SSE, not to
`text/event-stream`, and not to this server — `fetch()` can set any header it
likes and hand back the response body as a stream. Verified in the session's
Firefox against the running daemon, side by side:

| Approach | Result |
| --- | --- |
| `new EventSource('/api/v1/events?token=…')` — what ships today | closed, `readyState=2`, **0 events** |
| `fetch('/api/v1/events', {headers:{Authorization:'Bearer …'}})` + stream the body | **200**, events arrive |

Sketch, which is all `useEvents` needs:

```ts
const ac = new AbortController();
const r = await fetch(`${getBase()}/api/v1/events`, {
  headers: { Authorization: `Bearer ${getToken()}` },
  signal: ac.signal,
});
const rd = r.body.pipeThrough(new TextDecoderStream()).getReader();
let buf = "";
for (;;) {
  const { value, done } = await rd.read();
  if (done) break;
  buf += value;
  let i;
  while ((i = buf.indexOf("\n\n")) >= 0) {
    const frame = buf.slice(0, i); buf = buf.slice(i + 2);
    for (const line of frame.split("\n"))
      if (line.startsWith("data:")) queryClient.invalidateQueries();
  }
}
```

Nothing is given up by dropping the native `EventSource` here:

- **Auto-reconnect** — `useEvents` already ignores it. Its `onerror` closes the
  stream and retries on its own 5s timer, so the retry loop it has today carries
  straight over; it only needs an `AbortController` in the cleanup where it
  called `es.close()`.
- **`Last-Event-ID` resumption** — nothing to resume. `sse_handler` emits
  `SseEvent::default().data(...)` only: no `id:`, no event names, just `data:`
  frames off a broadcast channel plus keep-alive comments.
- **Cross-origin** — not a factor. `shepherd-http` installs **no CORS layer at
  all**, so a browser could never reach a different-origin `apiBase` for the
  ordinary RPCs either; the dev server proxies `/api` for exactly that reason.
  The web UI is same-origin in practice.
- **Browser support** — streaming `fetch` bodies and `TextDecoderStream` are
  available in every browser this kiosk would run.

### Done

`useEvents` now reads the stream with `fetch`, and `sseUrl` is replaced by
`openEventStream(signal)` in `client.ts` — which owns base + token exactly like
the axios interceptor does. `require_auth` is untouched: no credential in a URL,
no widening of the auth surface, and one fewer way for the two transports to
disagree about what counts as authenticated. No Rust changed.

Re-verified end to end against the rebuilt binary:

- **Requirement 2 now holds in the web UI.** Alternating `wpctl set-default`
  between the two devices moved the "In use now" chip, reordered the rows, and
  moved the volume readout (55% ↔ 25%) with the browser untouched. Before the
  fix the chip never moved at all.
- **A cap set elsewhere is no longer clobbered.** Daemon set to 42 externally →
  the browser rendered `Max 42%`; an off/on of its switch left 42 intact. Before
  the fix it rendered a stale 90 and wrote 90 back.
- **Cross-device, live.** Dragging the cap on the *phone* from 30 to 16 moved
  the daemon to 16, clamped the live volume 30% → 16% in hardware, and the
  untouched browser showed `Max 16%`. Phone, daemon, HUD and browser all agreed.
- **Reconnect works.** `sudo ss -K` on port 8080 killed the stream mid-session;
  the UI went stale as expected, then the hook's existing 5s retry re-opened it
  and picked up the next change with no reload.
- **The card's own paths still work through a browser**: switch on/off, a real
  pointer drag on the max slider (which set the cap to 30 and pulled the live
  volume 47% → 30% in hardware), and Forget on the inactive row.

`npx tsc --noEmit` clean; SPA rebuilt and re-embedded.

## The two UIs are not the same control

The per-device cap row exists twice — `shepherd-webui/.../AudioOutputsCard.tsx`
and `companion-android/.../ui/device/AudioOutputsCard.kt` — and looks identical
in both. It behaves differently in four ways. All four were measured live.

**1. The first-use default differs: 80 on the web, 50 on the phone.** Both hold
the slider position in local state seeded from the record, and each picks its own
fallback for a row that has no cap yet:

```tsx
const [draft, setDraft] = useState(record.max_volume ?? 80);   // web
```
```kotlin
private const val DEFAULT_CAP = 50f                            // companion
```

Turning the limit on for a never-capped device gave **80** in the browser and
**50** on the phone. Neither reads the global `[service.volume]` ceiling, so
neither number means anything in particular; the web's 80 coinciding with
`config.example.toml`'s cap is a coincidence.

**2. Off→on restores on the web and resets on the phone.** The web effect
deliberately refuses to sync when the cap is cleared, so the draft survives:

```tsx
useEffect(() => { if (record.max_volume !== null) setDraft(record.max_volume); },
          [record.max_volume]);
```

Compose keys the state on the value instead, so clearing the cap **destroys** it:

```kotlin
var draft by remember(record.maxVolume) { mutableFloatStateOf(record.maxVolume?.toFloat() ?: DEFAULT_CAP) }
```

Measured: web 90 → off → on → **90**; phone 25 → off → on → **50**.

**3. The web's "restore" restores whatever it last saw, which need not be
current.** ~~Because of the event bug above, its draft only tracks changes made
in that same browser tab.~~ **Resolved** by the event fix: the draft now follows
changes made anywhere, so off→on restores a value that is actually current.
Before the fix this was what turned a cosmetic difference into the silent
overwrite described above — the companion's "forgetful" `remember` was, by
accident, the safer of the two.

**4. Only the web UI gates the row while a write is in flight.** The web passes
`busy={setOutputLimitMutation.isPending || forgetOutputMutation.isPending}` and
disables the switch, slider and Forget; the companion's card has no equivalent
(its `busyId` belongs to the Windows panel), so its slider stays live while a
set is outstanding and a fast second drag can be reset under the user's finger
when the first response lands. Visible in the web UI as the `busy` gate
swallowing key presses — five `ArrowRight`s moving the value by one.

### All four resolved

3 fell out of the event fix. The other three were fixed here, and a fifth
problem surfaced while fixing 2.

**1 — one default, 50.** Both sides now name a `DEFAULT_CAP` constant that
points at the other, and both hold **50**. The companion's was the deliberate
one (a named constant with a doc comment against the web's bare `?? 80`) and it
is the more protective starting point for a hearing-protection feature.
Switching a limit on clamps immediately, so the switch visibly does something
and switching it back off undoes it. Measured after the change: turning a limit
on for a never-capped device gave **50 in the browser** and **50 on the phone**.

**2 — off→on restores what you had, everywhere.** The companion adopted the
web's shape: state seeded once per row, then a `LaunchedEffect(record.maxVolume)`
that syncs *only while a cap exists*, so clearing one leaves the number intact.
Measured on the phone: dragged to 26 → switch off → switch on → **26**, where it
used to reset to 50. Re-checked in the browser: same, 26 restored.

**2b — Compose rows had no identity** (found while fixing 2). The card rendered
`outputs.forEachIndexed { … OutputRow(record, vm) }` with no `key`, so per-row
state was **positional**. The device sorts the active output first, so rows
reorder whenever the sink changes, and a remembered slider position would stay
with the slot rather than the device. `remember(record.maxVolume)` had been
hiding it — re-keying on every value change threw the state away often enough
that it rarely showed. Fixing 2 removes that accident, so each row is now
wrapped in `key(record.output.key)`, matching the web's `key={r.output.key}`.
Verified by alternating the active output with different caps on the two rows
and watching the labels follow the devices through each reorder.

**4 — the companion gates the card while a write is in flight.** New
`DeviceUiState.audioBusy`, set in a `try`/`finally` around both actions, and the
switch, slider and Forget are disabled while it holds. The window deliberately
spans the **refreshes as well as the write** (`joinAll(refreshAudioOutputs(),
refreshVolume())`): a row re-syncs its slider from the record that comes back,
so a second drag begun after the write resolved but before the list arrived
would be snapped out from under the finger. Verified by polling the
accessibility tree across a tap — `enabled="false"` on both row switches 0.8s
after the tap, with the labels still showing the old value, then `enabled="true"`
and the new labels once it landed. A screen recording shows the same window as
the greyed styling.

Nothing else moved: the companion still *hides* Forget on the active row where
the browser disables it with a tooltip. That one is a platform difference, not a
divergence — Android has nowhere good to put the tooltip.

## Choosing the active output

Added after the validation pass, at the maintainer's request: the management
surfaces could show which device was in use but not change it, so a parent could
set a headphone cap and then had to walk to the machine to actually move the
sound there.

### Shape

- `shepherd-host-api` — `VolumeController::list_outputs()` and
  `select_output(key)`, both defaulted. `list_outputs` returns empty and
  `select_output` returns an **error**, not a silent `Ok`: a caller that asked to
  move the audio and got success back would have no way to learn it never moved.
- `shepherd-host-linux` — `AudioTopology::output_by_key` and `node_id_of`, and a
  shared `set_default_sink(id)` that `audio_route.rs` now calls instead of its
  private copy. Selection re-resolves the node id from the dump it is used with
  and never keeps it; PipeWire recycles ids across restarts and re-plugs.
- `shepherd-api` — `AudioOutputRecord.available`, defaulted to **true** on the
  wire. A client talking to a daemon that predates the field offers the choice
  and lets the attempt fail loudly, rather than greying out every device it
  could actually switch to.
- `shepherd-management` — `select_audio_output(output_key) -> VolumeInfo`, and
  `list_audio_outputs` now enumerates everything **plugged in**, not just
  whatever is selected. That second part is load-bearing: discovery ran off the
  active output alone, so the one device you could see was the one you wanted to
  switch away from.
- Both UIs — a "Use this" control per row, in place of the "In use now" chip.
  Disabled and relabelled "Not connected" for a device that is not here.

### How the decisions came out

**Selecting is the same event as the hardware switching.** `select_audio_output`
runs `enforce_volume_ceiling()` and broadcasts, exactly as the watch loop does
for a jack insert or a dock. A capped output is therefore quiet on arrival
whether the parent chose it or the hardware did.

**A row can name a device that is not here, and that is the point.** Rows outlive
the hardware so a cap set on the headphones survives unplugging them. `available`
separates "remembered" from "here right now" so the UI can offer the limit but
not the switch.

**Choosing the output already in use is not an error.** Two parents on two phones
can tap the same row; the second gets the current reading back and nothing is
clamped or disturbed.

**The host's "not available" is not the daemon's.** `VolumeError::NotAvailable`
renders as "Volume control not available: …", which tells a parent the wrong
thing — volume control is fine, the device is gone. The service unwraps it to a
`BadRequest` carrying just the reason, and routes real backend failures to
`Internal`.

### Verified

665 workspace tests (six new service tests, one new host test), 42 Android tests
(two new wire tests), `clippy --all-targets -D warnings` clean, `cargo fmt`,
`config validate`, `tsc --noEmit`.

End to end against the two real devices, both surfaces:

- **Switching works from the browser** — clicking "Use this" moved the real
  default sink (`wpctl status` confirms), both directions, with the reported
  percent and ceiling following the device (Focusrite 25/25, built-in 40/80) and
  a toast naming where the sound went.
- **Switching works from the phone** — tapping "Use this" moved the hardware,
  applied that output's 25% cap, updated the phone's own Volume card to 25%, and
  swapped the chip and button between rows.
- **The cap applies on arrival.** The Focusrite was left at 100% while
  unselected with a 25% cap set; choosing it dropped the hardware to 25% in the
  same call.
- **A device that is not plugged in cannot be chosen.** A real USB unplug
  (`/sys/bus/usb/devices/1-3/authorized`) left the row in place with its 25% cap
  and `available: false`; the browser showed "Not connected — Last used just
  now" with the button disabled and the tooltip "Not connected right now", the
  phone showed a disabled "Not connected" button and "Audio device — not
  connected", and the RPC refused with `bad_request: audio output is not
  connected: …`. Replugging restored both.
- **Discovery no longer needs selection.** Verified in the service tests and
  live: a device that has never been the default is listed and choosable.

### The watcher now watches the whole topology

The first cut of the picker enumerated only inside `list_audio_outputs`, on the
reasoning that doing it on the 2s tick would cost a second `pw-dump`. **That was
wrong.** `observe()` already ran `pw-dump`, and `parse_pw_dump` already built the
complete output list — then `current_output()` picked one and the rest was
dropped when the temporary `AudioTopology` went out of scope. The second dump
existed only because `observe()`'s *return type* narrowed to the active output.

So the read path was reshaped rather than duplicated:

- `AudioSnapshot { status, active, outputs }` replaces the
  `(VolumeStatus, Option<AudioOutput>)` pair `observe()` returned. Everything in
  it comes from one look at the system, so no two fields can describe different
  moments — the property change 1 established, now extended to the device list.
- `list_outputs()` is gone; it was a second way to ask the same question.
- `ObservedAudioState` gained `present_keys`, **sorted**, so a plug or unplug is
  a change while a differently-ordered `pw-dump` is not.
- The tick records *every* present output, not just the active one.

Three call sites collapsed onto the single read as a side effect, and one of them
mattered: `get_volume` was doing `volume_restrictions()` (a `pw-dump`) plus
`get_status()` (a `wpctl` spawn) plus `current_output()` (another `pw-dump`) —
**three process spawns on the hot path**, since every client refetches it on
every event. It is now one.

### Verified

668 workspace tests (three more: a device appearing, a device disappearing, and
a reordered list *not* counting as a change), 42 Android tests, clippy, fmt,
`config validate`, `tsc`.

Live, against the real USB device:

- **The watcher alone records a device that never becomes the default.** With
  the Focusrite's row forgotten and the built-in pinned as default, replugging
  the hardware and then making *no RPC calls at all* put the row back in
  `shepherdd.db` — read out of a copy of the SQLite file, so the observation
  could not itself have caused the recording.
- **A plug and an unplug each produce an event.** Unplug at 08:14:18.248 → event
  at 08:14:18.702; replug at 08:14:28.282 → event at 08:14:30.712. Both events
  carry identical percent, ceiling and active output — nothing but the set of
  present devices had changed, which is exactly what would previously have gone
  unnoticed.
- **Both UIs follow it untouched.** The browser card moved to "Not connected"
  ~4s after the unplug and back ~4s after the replug; the phone did the same
  over BLE.

One inconsistency was introduced and fixed in passing: the browser disabled a
"Use this" button and explained itself in a caption while the phone relabelled
the button. Both now show a disabled button reading **"Not connected"**.

### Found while verifying, and fixed

`IpcServer::shutdown()` unlinked its socket path unconditionally, without
checking it still owned that socket. Stop a session and start another quickly
and the outgoing daemon deletes the path the incoming one has already bound: the
new session comes up healthy, `[OK] Headless session up` and all, while every
client sits in a reconnect loop logging `No such file or directory` and the HUD
paints `-%`. It cost a confusing ten minutes here, and it looked exactly like the
change under test having broken something.

The guard records the socket file's `(dev, ino)` after a successful bind and
removes the path on shutdown **only** if the same file is still there. Anything
else belongs to a daemon that has taken over, and deleting it would strand all
of that daemon's clients while it went on serving a socket nobody could reach.

`start()`'s unlink stays unconditional and is now commented as deliberate: a
daemon that crashed leaves its socket behind, and refusing to bind over it would
make a crash unrecoverable without manual cleanup.

Two unit tests, both confirmed to fail with the guard reverted: the overlap
itself, and a server that never bound refusing to touch the path at all.

Proven live with two real daemons on one socket path, which is the shape the dev
loop hits:

```
d1 bound inode 39778
d2 took the same path while d1 was alive -> inode 39796
d1 shut down:
  WARN shepherd_ipc::server: Another daemon has bound our socket path; leaving it alone
socket still present, inode=39796; d2 answered `health` over IPC
d2 shut down: socket removed
```

## Still not covered

- Bluetooth audio. The dev host has a BT radio but no BT audio device, so the
  `bluez5` branch of `kind` detection and the `api.bluez5.profile` route analogue
  remain unexercised.
- A device that classifies as `Headphones` or `Speakers`. Neither output here
  does — one is `line_out`, one is `unknown` — so the icon/label mapping for
  those kinds is unit-tested only.
- Latency. The 2s poll is still the bound; nothing here argues it needs to
  become `pw-mon`.

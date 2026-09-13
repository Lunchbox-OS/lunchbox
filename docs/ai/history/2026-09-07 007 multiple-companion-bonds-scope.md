# Scoping #149 — support multiple companion bonds

Prompt (2026-09-07): *"I gave you a second phone in preparation to work
on #149. First set it up for development, then scope it out."*

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/149>

> BLE management currently only supports being paired to *one* phone.
> That's fine for single-parent households, but breaks down when there
> are multiple caregivers or the child device travels between multiple
> homes.

This note is the survey, not a plan of record: what the single-admin
assumption actually touches, which of those are mechanical and which are
real decisions, and the one thing blocking the second phone from being
usable.

## Part 1 — the second phone

| Serial | Phone | Android | Bench state |
| --- | --- | --- | --- |
| `63251JEA305665` | Pixel 10a (`stallion`) | 16 / SDK 37 | The original dev phone |
| `ZY22F6Z6NT` | moto g power (2021) (`borneo`, XT2117) | 11 / SDK 30 | New |

Done, and written into the `companion-pairing` skill:

- **A udev rule for Motorola.** The `no permissions (missing udev
  rules?)` trap the skill documents is keyed on *vendor id*, and the
  existing rule only covered `18d1` (Google). `/etc/udev/rules.d/51-android.rules`
  now carries `22b8` as well. A new phone from a third vendor will need
  its own line.
- **`ANDROID_SERIAL`, not `-s`.** With both phones attached every bare
  `adb` call fails `error: more than one device/emulator`, and `pair.sh`
  calls bare `adb` throughout — `require_adb` included, so it aborts with
  the misleading "no adb device". `adb` honours `ANDROID_SERIAL`, so
  exporting it once retargets the whole script with no edit. `SHOTDIR`
  is shared and wants overriding per phone too.
- **No lock screen** (`locksettings get-disabled` -> `true`) and BLE
  present (`android.hardware.bluetooth_le`), so the phone is
  automation-ready on both counts.

The phones being unalike is a feature for this issue. The "Android asks
twice" consent ordering, the notification-shade mechanics and the
`pairingVariant` numbering in the skill are all Pixel/SDK 37
observations; a second vendor on a five-year-old stack is exactly the
coverage a multi-bond feature wants.

### `minSdk` was lowered 31 -> 30 (resolved)

The Motorola took exactly one major update and stops at Android 11 /
SDK 30, so the app refused to install:

```
Failure [INSTALL_FAILED_OLDER_SDK: … Requires newer sdk version #31 (current version is #30)]
```

Nothing in the dependency set needed 31 — Kable, Compose, DataStore and
security-crypto all floor at 21. The 31 was the *manifest permission
model*: `BLUETOOTH_SCAN` / `BLUETOOTH_CONNECT` with
`usesPermissionFlags="neverForLocation"` are Android 12 permissions.

Decision (Albert, 2026-09-07): lower the floor to 30. The issue's own
motivation — several caregivers, whatever handsets they own — argues for
it on the merits, not just for the bench. The change is three edits:

- `minSdk = 30` in `app/build.gradle.kts`.
- `BLUETOOTH`, `BLUETOOTH_ADMIN` and `ACCESS_FINE_LOCATION` in the
  manifest at `maxSdkVersion="30"`, so Android 12+ never requests them.
- A `Build.VERSION.SDK_INT` branch on the runtime permission list in
  `ui/App.kt` — requesting a permission the platform does not define
  returns a permanent denial, which would wedge the gate closed forever.

The accepted cost is an Android 11 support floor and a location prompt
for those users: `neverForLocation` cannot spare them, because the
platform has no way to express the promise before Android 12.

Verified on the phone: install, permission gate, home screen, and a scan
that lists `shepherd` at `8C:68:8B:41:02:DC` (-53 dBm) against a
headless dev session pinned to that adapter. Pairing itself was
deliberately not exercised — the dev stack is already claimed by the
Pixel, and a second bond is this issue's work rather than its setup.


## Part 2 — what single-admin actually touches

### The record and its store — mechanical

`AdminRecord` (`crates/shepherd-ble/src/admin.rs`) is singular, and
`AdminStore` serialises it as one `[admin]` table in `admin.toml`. The
plural form is a `Vec` and a `[[admins]]` array, with a back-compat read
for the `[admin]` shape already on disk on every claimed device.

`AdminRole` already exists as an enum with one `Admin` variant, and the
module doc says the schema "already accommodates additional roles". That
part of the groundwork is real.

`PendingUnbondStore` is *already* a `Vec<String>` and needs nothing —
except that `BleServer::new`'s reset-sentinel path loads a single record
and queues a single address, and must queue all of them.

### The HTTP token — a trait signature

`AdminAuthority::current_http_token() -> Option<String>` (one token,
`crates/shepherd-management/src/auth.rs`) has three consumers:

- `shepherd-http/src/auth.rs:173` — `constant_time_eq` against the
  presented bearer.
- `shepherd-http/src/auth.rs:159` — `is_open()`, which only asks
  "is there an admin at all".
- `webauth.rs:364` — `companion_available()`, likewise.

Each admin mints its own `http_token`, so the trait wants
`verify_http_token(&self, presented: &str) -> bool` plus
`has_admin(&self) -> bool`. Keeping the comparison *inside* the
implementation is the point: handing out a `Vec<String>` would push the
constant-time discipline onto every caller.

### Peer identity — the actual hard part

`ClaimMachine::authorize` today allows **any** peer that reaches it, and
its doc comment explains why: BlueZ reports the resolved identity
address on reconnect, but at claim time during pairing it presented the
random private address still in use mid-handshake, so a strict
comparison broke every reopen of the app. With one bond that is sound —
the link is MITM-protected and there is only one of them. With N bonds
it is the whole feature: a write has to be attributable to *which*
admin.

Making it worse, `request_characteristic` (`server.rs:1604`) synthesises

```rust
let peer = PeerIdentity {
    address: req.device_address.to_string(),
    address_type: "public".to_string(),   // hardcoded; bluer doesn't expose it here
};
```

Two candidate fixes:

1. **Capture the identity at bond completion, not at claim time.** The
   drift the comment describes is specific to the mid-handshake window;
   once `Device1.Paired` goes true BlueZ has resolved the RPA. The agent
   already watches for exactly that transition
   (`spawn_hide_when_paired_or_timeout`), so the identity address and
   its real address type can be read there and handed to the claim
   rather than inferred from the first write.
2. **Use the IRK** from `/var/lib/bluetooth/<adapter>/<device>/info`.
   Authoritative, root-only, and coupled to BlueZ's on-disk format.

(1) looks right; (2) is the fallback if resolution turns out to be
unreliable in practice. This is the item the crate README already
nominates as #149's job, and it should be settled before anything else
is built on top.

### The transport is single-session — the largest hidden cost

`TransportState` (`server.rs:1030`) holds **one** of everything: one
`FrameReader`, one `last_peer`, one `response_outbox`, one
`events_outbox`, one `epoch`, one `had_session`, one `BearerPin`. The
comments say so plainly ("v1 expects one admin connection at a time").

With two phones connected at once, today:

- Phone B's reads on the Response characteristic drain **phone A's**
  replies — the outbox is global and the read handler does not look at
  `req.device_address` (it has it, and ignores it).
- Interleaved writes stitch into one `FrameReader` and corrupt both.
- Phone B opening a session sends `id == 1`, whose handler explicitly
  clears both outboxes — discarding whatever A was mid-way through.

Two ways out:

- **Multiple bonds, one active session.** Keep the transport as-is and
  admit only one connected admin at a time, with an explicit error for
  the second rather than silent corruption. Matches the issue as
  written — caregivers who take turns, a device that travels — and is a
  fraction of the work.
- **Per-peer transport state.** Key reader, outboxes, epoch and bearer
  pin by peer. The correct end state, and much larger; the event
  forwarder in particular currently pushes into one events outbox that
  runs whether or not anyone is connected, and would need a fan-out.

Recommend the first, with the second as a follow-up issue, unless
simultaneous use is a requirement.

### Enrolment policy — the product decision

Today `claim` is TOFU and rejects the second peer with `AlreadyClaimed`.
Notably the device stays **pairable and discoverable while claimed**,
and the pairing agent auto-accepts every request with no claim
awareness; combined with `authorize` allowing any bonded peer, a second
phone that bonds and simply *skips* `claim` already reaches the full
management surface. That hole is masked by the app always calling
`claim` first. Whatever #149 does, it has to close it.

So "who may become the second admin" is the central question:

1. **TOFU forever** — anyone who completes Numeric Comparison. The
   anchor is physical presence at the TV. Simple, and wrong for this
   product: the child is also physically present.
2. **Approval by an existing admin.** #156 just built this exact shape
   for web login — `request_login` / `list_login_requests` /
   `approve_login_request` / `deny_login_request` / `poll_login`, with
   handles, poll tokens, expiry and a `sweep`. A second phone bonds,
   calls `claim`, gets `pending`, polls; the first phone shows a card
   and approves. The wire pattern, the UI idiom and the expiry machinery
   all already exist, and the companion already renders a login-request
   card.
3. **A device-side enrolment code**, like `WebAuth`'s first-run code on
   the TV.

(2) as the primary, (3) as the recovery path for when no existing admin
is reachable — otherwise the only way back from a lost phone is a
factory reset. (1) stays as-is for the *first* admin.

### Revocation and reset

`factory_reset` currently means "clear the one record, forget the one
bond". Plural, that splits into at least: remove *this* admin
(self-unenroll), remove *another* admin (revoke), and wipe everything.
Each needs its own bond removals queued through `PendingUnbondStore`,
which already handles a list.

### Companion app

Structurally the app is fine — it already holds many *devices*
(`ShepherdRecord`, the device picker), and `role` is already carried on
the record. What it lacks:

- `ALREADY_CLAIMED` exists in `Rpc.kt`'s error enum and is never
  matched, so the second phone shows a generic failure today.
- A pending-enrolment state in `PairingScreen` ("waiting for approval on
  the other phone", polling).
- An approve/deny card for the existing admin, mirroring the web login
  request card.
- An admins list with revoke, in Settings.

## Suggested order

1. Settle peer identity (bond-completion capture) and add an
   identity-matching `authorize`, still single-admin. Closes the
   any-bonded-peer hole on its own.
2. Pluralise the record, the store and `AdminAuthority`, with migration.
3. Enrolment flow — approval by an existing admin, mirroring #156.
4. Refuse (rather than corrupt) a second concurrent session.
5. Companion UI: pending state, approve/deny, admins list, revoke.
6. Per-peer transport state — separate issue.

Steps 1–3 are where the design risk is; 4–5 are follow-through.

## Open questions

Settled with Albert on 2026-09-07:

- **`minSdk` -> 30.** Done; see above.
- **Turn-taking, not simultaneous connections.** One active session, with
  an explicit error for a second connected admin instead of today's
  silent corruption. Per-peer transport state becomes a follow-up issue
  (step 6), not part of #149.
- **Enrolment is by approval from an existing admin**, mirroring #156.
  TOFU stays for the *first* admin only.

Still open:

- **Recovery when the last admin phone is lost** — is a TV-displayed
  enrolment code acceptable, or is factory reset the intended answer?
- **Do the admins differ?** `AdminRole` has room for it. A "can view but
  not change limits" role is a plausible want for a babysitter, and
  costs little if the role is threaded through from the start — but
  nothing in the issue asks for it.

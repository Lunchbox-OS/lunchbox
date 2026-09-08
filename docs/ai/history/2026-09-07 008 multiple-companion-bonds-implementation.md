# Implementing #149 — multiple companion bonds

Prompt (2026-09-07): *"plan looks reasonable, build it"*, against the scope in
<2026-09-07 007 multiple-companion-bonds-scope.md> and the three decisions taken
there: lower `minSdk` to 30, turn-taking rather than concurrent sessions, and
enrolment by approval from an existing administrator.

Branch: `feat/149-multiple-companion-bonds`.

## What shipped

Roughly the suggested order, and it held up:

1. **Peer identity, and an `authorize` that compares.** `PeerIdentity` carries
   both addresses BlueZ has for a peer; the gate matches either.
2. **The roster.** `admin.toml` grew `[[admins]]`, with the v1 `[admin]` table
   read and rewritten on load. `AdminAuthority` became
   `verify_http_token` + `has_admin`.
3. **Enrolment by approval**, mirroring #156: `claim` answers `claimed` or
   `pending`, and `list_enrolment_requests` / `approve_enrolment_request` /
   `deny_enrolment_request` / `list_admins` / `revoke_admin` sit beside it.
4. **Turn-taking.** One session owner; a second writer gets `InProgress`, a
   non-owner reads empty.
5. **Companion UI.** A waiting-for-approval dialog on the requesting phone, an
   Administrators screen on the approving one.

Per-peer transport state (step 6) stayed out, as agreed.

## The three things the bench found that the plan did not

Every one of these was invisible to the unit tests and would have shipped.

### 1. A peer has two addresses, and the plan picked the wrong one

The scope said the identity question was the crux and proposed capturing the
address at bond completion. Before building that I checked the claim on the
dev stack against BlueZ, and the record's `identity_address` matched the address
a reconnecting Pixel's writes arrived from — so I concluded a strict address
comparison was sound as-is and dropped the resolution work.

That conclusion was right about the Pixel and wrong in general. When the
Motorola claimed, the daemon logged:

```
A second phone asked to administer this device peer=79:C2:1B:08:F0:32
```

while its bond was already being written to `/var/lib/bluetooth/.../64:11:A4:B0:7B:D9`.
A GATT request names its peer by **D-Bus object path**, which for a phone using
privacy is the random address the link came up on; `Device1.Address` on the same
object is the **identity address**. They are equal often enough to fool a single
sample — for a dual-mode phone reached over its LE identity, the path *is* an
identity — and different exactly when a phone claims mid-pairing, which is the
normal case for a second phone.

Recording the path address would have written an administrator that could never
authorize again. `resolve_peer` now reads `Device1.Address` once per session
(bluer exposes it as `Device::remote_address`, documented as "Identity Address
after pairing"), records hold the identity, and `PeerIdentity::matches` accepts
either address so records written before this keep working. The Pixel's
migrated record is exactly that case, live:

```
Peer connected under a different address than its bond identity
    peer=78:61:DF:9B:2C:8E identity=B8:F4:A4:E5:20:F1
```

Its record holds the path address. Matching only the resolved identity would
have locked out the device's existing administrator on upgrade.

### 2. Removing a bond needs the path back

`Adapter::remove_device` addresses a device by object path. Revoking the
Motorola therefore logged:

```
BlueZ bond removal failed peer=64:11:A4:B0:7B:D9
    error=Bluetooth device does not exist: Does Not Exist
```

— the record's identity address, against an object living at
`dev_53_B5_E3_8D_D6_49`. Worse, the startup drain's "is this address in
`device_addresses()`?" check would then have read the same absence as *already
settled* and dropped the queue entry, leaving the bond forever with nothing
left saying it should not be there.

`device_path_for` translates identity → path by asking each known device for its
`remote_address`, and both the live unbond task and the startup drain go through
it. Verified: the queued entry survived a restart, resolved, removed the bond,
and cleared itself.

This bug predates #149 — a factory reset with a phone connected under a random
address would have hit it — but #149 made revocation a routine operation and so
made it reachable.

### 3. The protocol constant was hand-mirrored

Bumping `PROTOCOL_VERSION` to 2 on the device and not in the companion's
`Protocol.kt` compiles cleanly on both sides and surfaces only as a phone
refusing to pair, blaming *the app* for being out of date — on a phone that had
just been given the new build. `protocol_constants_match_the_companion` in the
codegen drift tests now asserts the version and the five GATT UUIDs against the
Rust definitions. (Verified it fails when they disagree, rather than assuming.)

## A correction worth recording

The first cut of the companion's stale-bond rule dropped a prior bond whenever
the phone held no local record for a claimed device, on the theory that an
unjustifiable bond is a stale one. That is wrong for exactly the case #149
creates: a second phone that bonded on a previous attempt and has not been
approved yet holds a bond the device also holds. Dropping it left the device
with a key the phone no longer had, and the next connect died mid-handshake as
`Pairing failed — Disconnect detected` with nothing on either side explaining
why. The rule is back to the original one — drop only when the device reports
itself *unclaimed*, which is the only state where the bond is provably stale.

## Turn-taking is enforced by the radio first

A peripheral stops advertising while a peer is connected, so the second phone's
scan lists nothing at all while the first holds the link — and BlueZ goes on
reporting the advertisement as registered (`ActiveInstances` = 1), so the daemon
looks perfectly healthy throughout. The transport-level rule (`InProgress` for a
second writer, empty reads for a non-owner) is therefore defence in depth rather
than the thing users hit. It is worth keeping — it is what stops the
single-session `TransportState` from being load-bearing on an accident of BlueZ
configuration — but it means the two-phone flow is inherently a relay: B asks, B
is parked, A approves, A is parked, B collects.

## End-to-end, on hardware

Against the headless dev stack pinned to `8C:68:8B:41:02:DC`, with a Pixel 10a
already claimed from before the change:

| Step | Result |
| --- | --- |
| v1 `admin.toml` migrated on load | `[admin]` → `[[admins]]`, token preserved, id minted |
| Pixel reconnects | authorized; every RPC `ok=true` |
| Motorola bonds and claims | `pending`, code `804038` on its screen |
| Pixel's Administrators screen | same code, `moto g power (2021) · 64:11:A4:B0:7B:D9` |
| Approve | two `[[admins]]`, distinct ids and tokens |
| Motorola reconnects | `claim` → claimed, then `service_state`; "Paired" |
| Motorola's Administrators screen | both phones, own row flagged |
| Motorola revokes itself | record gone; its next RPCs denied |
| Restart | queued unbond resolved and removed; bench back to one admin |

## Revalidated after rebasing onto main (2026-09-11)

Rebased clean over 29 commits, including administrator mode (#154) and the
config editor in web management (#185). Nothing conflicted, and re-running the
codegen produced no diff — the textual merge of the generated files already
matched.

The rebase mattered for one reason worth checking rather than assuming:
`ServiceStateSnapshot` gained `admin_mode` and `locked`, and that type crosses
the BLE wire. Both phones decoded it and rendered a full home screen.

Whole flow re-run on hardware from a restored v1 `admin.toml`: migration, the
Pixel reconnecting and authorizing, the Motorola enrolling by approval (code
`141495`, matching on both screens), both phones listing the roster, and a
revocation — this time of the *other* administrator rather than self, which
removed the bond **inline** rather than through the retry queue, exercising
`device_path_for` in the live path as well as the startup drain.

Two things that cost attempts and are now in the skill:

- A revoked phone keeps its local record and lands on **"Bond lost — re-pair
  needed"**, not the empty state, so `pair.sh tap "Pair a device"` finds
  nothing and the subsequent `run` waits on a scan nobody started — which reads
  as "the device isn't advertising".
- The earlier claim that `pair.sh run` does not work on the Motorola was a
  **wrong diagnosis**. It failed on a phone whose bond state was broken; with
  both sides clean it drives Android 11's prompts fine. It still "times out" on
  a second phone, because it waits for `Paired` and an unapproved phone stops
  at "Waiting for approval" — success, read from the daemon log rather than the
  exit code.

One inert edge noticed while re-running: a phone that is revoked and then
re-approved keeps the `http_token` its *old* record carried, because the
companion only calls `claim` during pairing and reconnected here on the bond
alone. BLE access is unaffected — `authorize` compares addresses, not tokens —
and the app never reads `ShepherdRecord.httpToken` back, so nothing observable
depends on it today. It would matter the moment the app offers that token to a
human.

## Left undone

- **A denied or abandoned enrolment leaves the requester's BlueZ bond.**
  `authorize` refuses it, so it is a nuisance rather than a hole, but the device
  accumulates bonds from phones it said no to. Unbonding on denial was
  considered and dropped: the refusal is delivered on the requester's *next*
  poll, and removing the bond first takes away the link that would carry it.
- **Per-peer transport state**, as agreed — a separate issue.
- **Recovery when the last admin phone is lost** is still `factory_reset`. The
  scope raised a TV-displayed enrolment code as the alternative; nothing here
  changes that answer.
- **`revoke_admin` has no web-UI counterpart.** The roster RPCs live in
  `shepherd-ble` rather than on `ManagementService`, because they act on the
  claim machine and `shepherd-management` sits below it. Reaching them from a
  browser needs the trait split that avoids the cycle.

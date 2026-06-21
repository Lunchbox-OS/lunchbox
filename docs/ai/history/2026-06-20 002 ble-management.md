# BLE Management Interface (#65)

> Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/65>
> "Management interface when network is down"

## Motivation

The existing HTTP management API (`shepherd-http`, see
`2026-04-25 002 http management api.md`) requires:

1. an active network connection between phone and shepherd,
2. either mDNS/Bonjour autodiscovery or a static IP the admin remembers, and
3. the LAN to actually work (DHCP, no AP isolation, no captive portal, etc.).

In practice points 1–3 fail often enough that "I need to change a setting and
can't reach the box" is a real recurring problem. The issue suggests two
remedies: a WiFi hotspot fallback, or management over BLE.

This document specifies the BLE path. The hotspot path is **rejected as the
primary** because BLE removes the IP-discovery / static-IP friction entirely and
works regardless of network state, which makes it suitable as the *primary*
admin transport, not just a fallback. HTTP remains available but secondary.

## Verified hardware capabilities

`bluetoothctl show` on the target hardware (Marvell HCI 4.0 USB controller,
`hci0`) reports:

- `Roles: central` and `Roles: peripheral` — can act as a GATT server.
- 5 supported advertising instances, 31-byte adv + 31-byte scan-response.
- LESC (LE Secure Connections, BLE 4.2+) supported.
- BlueZ D-Bus API works without `sudo` for the `bluetooth` group.

The `bluer` crate (BlueZ D-Bus bindings) is the planned dependency for the
GATT server and pairing agent.

## Two-layer auth model

BLE bonding and app-layer authorization are independent concerns and are
deliberately separated:

- **Link layer** — confidentiality + peer identity. Handled by BLE pairing +
  bonding. Once bonded, BlueZ encrypts the link and stores an LTK + IRK in
  `/var/lib/bluetooth/`. The IRK is the stable peer identifier; the BLE MAC
  rotates (Random Private Address) but the IRK does not.
- **App layer** — who is allowed to do what. Handled by a per-bond admin
  record (IRK → device name + bearer token + role) stored in shepherd's state
  directory.

## Auth choices

### Link layer: Numeric Comparison (LESC)

- 6-digit code shown on both sides; user confirms match on the phone.
- Authenticated key exchange — MITM-resistant.
- Requires a display on both sides. **Commits us to "TV/Sway must be up
  during pairing."** A device that crashes before Sway is up cannot be
  re-paired; recovery requires the filesystem reset path (below).

Rejected alternatives:

- **Just Works** — anonymous ECDH, MITM-vulnerable during the pairing window.
  Unacceptable for a device that controls system policy.
- **Passkey Entry** — equivalent display requirement, slightly worse UX
  (user types the code instead of confirming).
- **OOB (QR code on a sticker)** — viable and removes the display
  requirement, but requires printing/regenerating a physical artifact and
  adds a QR-generation path to the admin app. Defer; revisit if "TV must be
  up during pairing" turns out to be a real blocker.

### App layer: TOFU, single admin (v1)

- First phone to complete pairing + the `claim` RPC inside the initial setup
  window becomes the sole admin.
- Subsequent claim attempts from other phones are rejected
  (`already_claimed`).
- Multi-admin (approve-new-device from an existing phone) is explicitly
  deferred to v2, but the per-bond record schema is designed to support it
  without migration.

Rejected alternatives:

- **Pre-shared enrollment code** — requires the user to retrieve a code from
  some other channel before claiming, which reintroduces the very network
  dependency we are removing.
- **Admin-signed enrollment** — overkill for a single-household device; the
  right design when we have multiple admins, not at v1.
- **Static password** — phishable, leaky, no upside over TOFU.

## GATT layout

One custom 128-bit service ("Shepherd Management Service"). UUID to be
generated at implementation time and committed to a `shepherd-ble-protocol`
constants module.

| Characteristic | Properties              | Purpose                                                                                  |
|----------------|-------------------------|------------------------------------------------------------------------------------------|
| `DeviceInfo`   | read                    | Claim state, firmware version, protocol version. Readable pre-pairing (unencrypted).     |
| `Request`      | write                   | Client writes length-prefixed JSON-RPC frames. Chunked across writes if > negotiated MTU.|
| `Response`     | notify (encrypt-req)    | Server pushes JSON-RPC responses; correlated to requests by `id` field.                  |
| `Events`       | notify (encrypt-req)    | Mirror of the existing HTTP SSE stream — state changes, session events, etc.             |

`DeviceInfo` is intentionally readable without encryption so the companion
app can present a useful "this device is unclaimed; tap to set up" UI before
initiating pairing.

`Request`, `Response`, and `Events` require an encrypted (bonded) link. The
RPC payload schema is the same JSON the HTTP API uses — see "Unified
identity" below.

### Framing

- Each frame is `u16 length | JSON bytes`.
- Frames > negotiated ATT MTU are split across multiple writes; the reader
  reassembles based on the length prefix.
- Each request/response carries an `id` field for correlation; out-of-order
  responses are fine.

## Claim state machine

```
            ┌──── reset sentinel at startup ──────┐
            │     (or factory_reset RPC)          │
            ▼                                     │
        Unclaimed ──pair + claim RPC──▶ Claimed ──┘
```

- **Unclaimed**: accepts a pairing connection. The only RPC the `Request`
  characteristic honors is `claim`; everything else returns `not_claimed`.
- **Claimed**: only requests from the bonded peer whose IRK matches the
  admin record are honored. Other bonded peers (none expected in v1) get
  `permission_denied`.

## Admin record

Stored as a single TOML file alongside other shepherd persistent state
(exact path TBD — should match wherever the rest of state lives):

```toml
[admin]
irk         = "<base64>"           # stable identifier — survives MAC rotation
device_name = "Albert's iPhone"    # supplied by client in the claim RPC
bonded_at   = "2026-06-20T22:30:00Z"
http_token  = "<random 32 bytes, base64>"  # also valid for the HTTP API
role        = "admin"              # forward compat for multi-role v2
```

The IRK is the source of truth. When a peer connects, BlueZ resolves the
peer's random address to its IRK; we look up the record from there.

## Numeric Comparison flow

1. Phone scans for shepherd by advertised name + service UUID.
2. Phone reads `DeviceInfo` (unencrypted), sees `state = "unclaimed"`,
   prompts the user to pair.
3. Phone initiates pairing — LESC required, MITM required, IO capability
   `DisplayYesNo`.
4. BlueZ raises `RequestConfirmation` on D-Bus; the `bluer` agent in
   shepherd receives the 6-digit code.
5. Shepherd renders the code as a Sway overlay (full-screen, large font)
   via `wlr-layer-shell`.
6. User confirms the match on the phone.
7. Bond completes; BlueZ persists LTK + IRK.
8. Phone sends `claim { device_name }` over the now-encrypted link.
9. Shepherd writes the admin record (with IRK + minted HTTP token), returns
   the token in the `claim` response. Phone stores the token.

After this, the phone reconnects without any user interaction. The HTTP
token is independently usable over the LAN if the phone happens to be on
the same network.

## Unified HTTP + BLE identity

- BLE `claim` becomes the *only* way to provision an admin in v1.
- HTTP requests authenticate by bearer token; the token must equal
  `admin.http_token`. The legacy static-token path in `ManagementApiConfig`
  is removed, or downgraded to a config-only debug override (decision
  deferred — flag at implementation time).
- Single source of truth: clearing the admin record (or factory reset)
  invalidates both the BLE bond and the HTTP token in one step.

## Filesystem reset

- A sentinel file at a well-known path under shepherd's state directory.
- On startup, if the sentinel exists:
  1. Call BlueZ `RemoveDevice` for the admin's bonded peer.
  2. Delete the admin record.
  3. Delete the sentinel.
  4. Set state to `Unclaimed`.
- Documented in `docs/INSTALL.md` (or a new troubleshooting doc): "If you
  lock yourself out, SSH in, `touch <path>`, restart `shepherdd`."

This is acknowledged to require shell access, which somewhat undercuts the
"BLE is the only admin path" stance. It is treated as a true last-resort
recovery, not a routine operation. A future hardware-button reset path may
replace it.

## Work breakdown

Implementation should proceed in this order; each step is independently
reviewable.

1. **Refactor — extract `ManagementService` trait** from `shepherd-http`
   handlers. Both HTTP handlers and the new BLE handlers call into the same
   service. This is the load-bearing prerequisite; without it the BLE
   implementation either duplicates handler logic or accumulates a parallel
   business-logic layer. Mostly mechanical, no behavior changes; should be
   merged as a standalone PR before the BLE work begins.

2. **New crate `shepherd-ble`**. Depends on `bluer`. Contains:
   - GATT service + characteristic definitions
   - `bluer` Agent implementation (Numeric Comparison via Sway overlay)
   - Claim state machine
   - RPC framing / chunking
   - Admin record persistence (file IO)
   - Reset-sentinel check at startup

3. **Sway passkey overlay**. A `wlr-layer-shell` surface that pops up
   during pairing and dismisses on confirmation. Lives in
   `shepherd-launcher` (or wherever the Sway-side UI code lives —
   determine at implementation time). Triggered via an in-process channel
   from the `bluer` agent.

4. **Wire-up in `shepherdd`**. Spawn the BLE service alongside the HTTP
   service. Both share `AppState` and the broadcast channel for events.

5. **Companion app (iOS + Android)**. Separate project; out of scope for
   this repo. Reuses the same JSON-RPC schema as the HTTP API.

6. **Docs**. Pairing flow, reset procedure, security model. Update
   `docs/INSTALL.md` and add a new admin-guide page.

## Open questions / followups

- **Sentinel file path** — pick at implementation time to match where the
  rest of shepherd's persistent state lives.
- **Static-token HTTP auth fate** — fully remove, or keep as a config-only
  debug override.
- **Display fallback** — Numeric Comparison commits us to Sway being up at
  pairing time. If this becomes a practical problem, the OOB-QR path
  (printed sticker, or QR shown on device) is the planned escape hatch.
- **Multi-admin / approval flow** — deferred to v2. The per-bond record
  schema already accommodates multiple entries plus a `role` field.
- **GATT service UUID** — generate and commit a stable UUID before the
  companion app is built.

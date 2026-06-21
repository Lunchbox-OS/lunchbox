# shepherd-pairing-display

Sway / `wlr-layer-shell` overlay that renders the 6-digit BLE Numeric
Comparison passkey during pairing.

Launched as a subprocess by `shepherdd` when the BLE pairing agent
fires `RequestConfirmation`; lives until killed (typically a few
seconds later, when the pairing window closes). Single full-screen
overlay on the compositor's overlay layer with the passkey in a very
large font and a short instruction to compare the number against the
companion app's display.

See `docs/ai/history/2026-06-20 002 ble-management.md` (Numeric
Comparison flow) for the pairing model this fits into.

## Invocation

```
shepherd-pairing-display --passkey 123456 --device "AA:BB:CC:DD:EE:FF"
```

Both flags are required. `--passkey` is the 6-digit number BlueZ
hands the agent. `--device` is shown verbatim — `shepherdd` passes
the bonded peer's address; future versions can pass a friendly name
once one is available.

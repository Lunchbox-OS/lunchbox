# shepherd-pairing-display

Sway / `wlr-layer-shell` overlay for the two six-digit numbers a parent has to
read off the television. Launched as a subprocess by `shepherdd` and killed
when whatever it announces is over.

Two modes, and the difference in how much screen they take is the whole design:

| Mode | Shape | Why |
|---|---|---|
| `--passkey` | Full-screen | BLE pairing is happening *now* and lasts seconds; the person at the TV must not miss it |
| `--setup-code` | Corner card | The web setup code (issue #156) is up for minutes while a parent finds a browser, and the child may be mid-activity behind it |

The full-screen mode's window paints the backdrop; the corner card's window is
transparent and the card paints its own, or it would black out the activity it
is sitting on top of. Neither grabs the keyboard: the person is acting on a
phone or a laptop, not on this screen.

See `docs/ai/history/2026-06-20 002 ble-management.md` (Numeric Comparison
flow) for the pairing model, and
`docs/ai/history/2026-09-07 003 web-management-authentication-scope.md` for the
setup flow.

## Invocation

### BLE pairing

```
shepherd-pairing-display --passkey 123456 --device "AA:BB:CC:DD:EE:FF" --method compare
```

All three are required. `--passkey` is the 6-digit number BlueZ hands the
agent. `--device` is shown verbatim — `shepherdd` passes the bonded peer's
address. `--method` (`compare` | `enter`) selects the instruction copy, so it
matches what the phone is actually asking for.

### Web management setup code

```
shepherd-pairing-display --setup-code 419624 --url https://192.168.1.10:8080
shepherd-pairing-display --setup-code 419624 --port 8080     # wildcard bind
```

`--url` when the daemon knows its own address; `--port` when it binds a
wildcard and there is no single address to name, in which case the card says
"port 8080 on this device" rather than inventing a hostname. Neither is
required — without both, the card falls back to naming no address at all.

Unlike the pairing passkey, the setup code is **not** selectable: GTK renders a
selectable label pre-selected, and a fully highlighted number on a small card
reads as an error state.

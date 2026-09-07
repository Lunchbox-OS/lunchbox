# Network status page in the management UIs (#182) — implementation

**Prompt:** "build it", following "scope out #182"
(see [`2026-09-07 003 network-status-scope.md`](./2026-09-07%20003%20network-status-scope.md)).

**Issue:** <https://git.armeafamily.com/albert/shepherd-launcher/issues/182>

Read-only network status — the WiFi network, the addresses, the connectivity
checks, and where the web interface is listening — on both management UIs.

## What the scope predicted, and what it got wrong

The scope was right that the management surface makes a new read-only method
nearly free: `#[management_rpc]` generated the dispatcher, both transports are
pass-throughs, and the wire codegen produced the TypeScript and Kotlin types.
Adding `network_status()` to the trait made it reachable from the browser *and*
the phone with no route, no handler, and no auth work.

Two things it did not anticipate:

1. **`schemars` renders a fieldless enum two different ways.** A unit-only enum
   with no documented variants becomes `"enum": [...]`; one with *every*
   variant documented becomes a `oneOf` of string `const`s. Document only some
   and it emits a mix, which the Kotlin renderer refuses — with a message
   naming the wrong fix (`add it to HAND_WRITTEN`). Now written down in
   `CONTRIBUTING.md` under "Generated client types".
2. **The web UI has no service snapshot.** The scope said the connectivity
   checks "need no new plumbing at all" because they ride
   `ServiceStateSnapshot.internet_status`. True of the companion, which holds a
   snapshot; not true of the browser, which queries per page and had no
   `service_state` call at all. Rather than duplicate the checks onto
   `NetworkStatusView` — two sources for one fact — the web client gained a
   `getServiceState()` whose only caller is this page.

## Shape

```
LinuxNetworkInfo (NetworkManager over D-Bus, getifaddrs fallback)
  → NetworkSnapshot                      raw parts, per host
      +
    WebListenerHandle                    what the listener is really doing
  → NetworkStatusView::new(…)            every judgement, made once
  → network_status()                     one RPC, both transports
      → NetworkPage.tsx / NetworkScreen.kt
```

`NetworkStatusView::new` is where `reachable`, the interface ordering, the cap,
and the management URLs are decided. A provider hands over what the host said
and nothing more, so a second host implementation cannot quietly disagree with
the first — and every one of those decisions is unit-tested without a D-Bus
daemon.

### What "reachable" means, and why it is derived

The dev device's real interface list is the argument for this field:

```
lo                       Loopback  127.0.0.1/8
wlx28187845b61d          Wifi      192.168.0.139/24  ssid=… gw=192.168.0.1
lxcbr0                   Bridge    10.0.3.1/24
ztks5unao4               Vpn       172.27.154.85/16
p2p-dev-wlx28187845b61d  Other     (down, no address)
```

Five interfaces, four addresses, and exactly two of them are a way in. A page
that lists all five equally makes a parent pick, and `10.0.3.1` looks as much
like an answer as `192.168.0.139` does. So loopback and container bridges are
never reachable, a VPN always is (on a remotely administered device it is often
the *only* one that works), and an unrecognised device type is treated as
reachable — being wrong about a veth costs a line in a list, while being wrong
about a real interface costs the address somebody needed.

IPv6 link-local is listed but never offered as a URL: `fe80::…` needs a zone
index, and the phone's zone is not the device's.

### The web listener

`HttpServer` now publishes its real state into a `WebListenerHandle` — the one
piece of this that is not a read-out. It reports the address it *bound*, not
the one it was configured with, which differ under `port = 0` and while
`bind_retry_seconds` is still waiting for an address to appear.

A listener that gave up now raises `DiagnosticCode::ManagementApiUnavailable`,
so it shows on both existing Health screens with a remedy. Before this, a
device whose management API never bound was indistinguishable from a healthy
one in every UI — the failure reached `error!(…)` and stopped there, on a
device whose web interface is precisely how somebody would have read that log.
It is a `Warning`, not a `Critical`: the companion is on BLE and unaffected, so
this is one path lost rather than a device lost.

Deliberately not raised while a bind is still being retried. A VPN interface
coming up at login would otherwise alarm every boot and clear seconds later.

### Freshness

Polling, as the scope recommended: 10s while either page is open. NetworkManager's
`StateChanged` is already watched in `shepherdd/src/system_events.rs`, so a
`NetworkChanged` event is available later — but a new `EventPayload` variant is
a wire change both clients must handle, and it costs BLE frames on a device
where nothing is looking at the page.

## Verified

* `from_network_manager()` against the real system bus, as the unprivileged
  user, via an `#[ignore]`d test in `shepherd-host-linux` — SSID, signal (60%),
  band (5220 MHz), addresses, gateway, DNS, ZeroTier typed as `Vpn`, `lxcbr0`
  as `Bridge`. Every property is readable with no polkit rule and no
  configuration; this is the assertion no unit test can make.
* `interfaces_from_kernel()` in a plain unit test — it must work in a CI
  container with no NetworkManager, which is exactly when it is load-bearing.
* URL derivation, ordering, the cap, and the "not serving offers no URL" rule
  in `shepherd-api`; the RPC end to end through `dispatch_json` in
  `shepherd-management/tests/dispatch.rs`.
* The web page itself, rendered by a real browser inside the headless dev
  session against the real daemon: the two management URLs
  (`http://192.168.0.139:8080` and the ZeroTier `http://172.27.154.85:8080`),
  the three connectivity checks read off the service snapshot, the wireless
  card reading "Connected to DIRECT-04-HP OfficeJet 250 — 60% signal · 5 GHz",
  and "Show 3 other interfaces" folding away loopback, `lxcbr0` and the
  down `p2p-dev-*` device.
* Five jsdom tests in `shepherd-webui/src/pages/NetworkPage.test.tsx` covering
  the three judgements a parent acts on — which address is offered first, that
  a listener which is not serving offers none, and that "could not read the
  network" and "offline" are different sentences.

### Driving a browser in the headless session

Two things cost time here and are worth writing down:

* **`dev click` does not work on a web page either.** The skill documents this
  for GTK4 widgets; the synthetic pointer does not activate a link or a nav
  item in Firefox either. To render a specific SPA page, temporarily change
  `App.tsx`'s initial `useState<Page>` and rebuild `dist/` — the SPA has no URL
  routing to deep-link with. `wtype -M ctrl -k minus -m ctrl` (zoom out) is a
  working substitute for scrolling, which `Page_Down` cannot do without content
  focus.
* **Firefox is a snap.** `--profile` pointing anywhere outside
  `$HOME/snap/firefox/common` fails with "Your Firefox profile cannot be
  loaded", and the half-started instance then holds a lock that makes the next
  launch report "already running, but is not responding". Use the default
  profile.

## Out of scope, as filed

Any wifi (re)configuration — joining, forgetting, toggling an adapter. MAC
addresses and BSSIDs are also deliberately absent: they are the fingerprintable
part and serve neither "SSH in" nor "open the web UI".

## Relationship to #156

None, in the end. Nothing here changes the auth model: the data is visible to
callers already authorized to launch sessions and read usage. #156 remains open
and this page adds SSID and internal addressing to what a sniffer on the same
LAN already gets from the bearer token — which makes #156 slightly more
valuable, and did not block this.

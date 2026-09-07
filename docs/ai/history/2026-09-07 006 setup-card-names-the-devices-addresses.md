# Web management authentication (issue #156) — rebase onto #182, and the
# setup card learns where the device is

> Status: **rebased, reverified, and extended.** The branch now sits on `main`
> at `6126553` (the network status page, #182). Everything the previous BLE
> pass covered was re-run on hardware against the rebased tree, and the
> first-boot setup card now names the device's real addresses instead of
> asking the parent to already know one.
>
> Earlier history: `2026-09-07 003 web-management-authentication-scope.md`,
> `… 004 …-handoff.md`, `… 005 …-ble-reverification.md`.

## Prompt

> #183 is checked out. Refetch, rebase on main, and reverify. You still have
> the phone to test BLE scenarios. Use the network-status awareness that was
> just added to include the listening IP addresses on the first-boot popup

## The rebase

`main` had moved from `1393cfa` to `6126553`, picking up **#182, "Network
status page in the management UIs"** — ten commits that add a `NetworkStatusView`
wire type, a NetworkManager-backed provider, a `WebListenerHandle` the HTTP
server publishes its real state into, and a network page in both UIs.

Five commits conflicted. Every conflict was **additive on both sides** — #182
and #156 each add a field to `ManagementServiceDeps`, a builder call on
`HttpServer`, a `let` before the HTTP server is constructed, a button on the
companion's Device controls screen — so the resolutions are unions, and the
generated files (`WireTypes.generated.*`, `rpc-methods.generated.ts`,
`RpcMethods.kt`) were regenerated rather than merged:

```sh
cargo run -p shepherd-wire-codegen --bin rpc-codegen
```

### The one conflict that a union got wrong, and how it hid

`ShepherdViewModel.kt` and `DeviceControlsScreen.kt` both conflicted at a point
where **the two sides shared a trailing token**: a `/**` opening one KDoc block
in one case, a closing `}` on an `OutlinedButton` in the other. Concatenating
"ours" and "theirs" therefore produced Kotlin that does not parse —

```kotlin
        }
    }
     * Re-read the web UI's password state and any browsers waiting for a tap
```

— and `cargo check --workspace --all-targets` was perfectly happy about it,
because none of it is Rust. It surfaced only when the companion was built, well
after the rebase had finished, as 200 lines of
`Syntax error: Expecting member declaration`.

**Build the companion as part of finishing a rebase that touched it**, not as
part of the verification pass afterwards:

```sh
cd companion-android && ANDROID_HOME=/opt/android-sdk ./gradlew :app:assembleDebug
```

(The SDK is at `/opt/android-sdk`, not `~/Android/Sdk`, and nothing in the repo
sets `ANDROID_HOME` for you — a bare `./gradlew` fails with "SDK location not
found".)

The fix was folded back into the commit that introduced it
(`git commit --fixup <sha>` + `git rebase -i --autosquash`), so no commit on the
branch has un-compilable Kotlin in it.

## What #182 and #156 do to each other

Two interactions, one a bug and one an opportunity. Both are fixed here.

### `management_urls` was handing out `http://` for an `https` listener

`NetworkStatusView::management_urls` derives the URLs that should reach the web
interface, and hardcoded the scheme. That was correct when #182 was written and
is wrong on this branch: since TLS termination landed, **any bind that is not
loopback serves HTTPS**, and a TLS listener answers a plaintext request with a
connection reset rather than a redirect. So the network page — on the phone, the
one screen whose entire purpose is to be useful when the network path is broken
— was offering a URL that cannot open.

`WebListenerView` gains `tls: bool`. It is written by whoever owns the listener
rather than read from the config, for the same reason the bound address is:
`tls.mode = "auto"` resolves against where the bind actually landed, so the
config is an intention and the listener is the outcome. Neither UI needed a
change; both render `management_urls` verbatim.

### The setup card can now say where to go

Before: a wildcard bind (which is what `config.example.toml` and every real
device use) gave the card no address, so it said

> Open this device's address in a browser on port 8080, and enter this code to
> choose a password.

— which asks a parent to already know an address for a device they have not set
up yet. The card had only the config's `bind`, and `0.0.0.0` is not an answer.

After, on this box:

> Enter this code at this address on your phone or laptop to choose a password:
> **https://192.168.122.130:8080**

The card asks `ManagementService::network_status()` for it, so it inherits every
judgement #182 already makes and tests: loopback and container bridges are not
ways in, a down interface is not a way in, an IPv6 link-local is not a way in,
one URL per interface rather than one per address, and IPv4 first because that is
the one somebody can read off a television and retype.

Three shapes, all rendered against a live session and screenshotted:

| The daemon knows | The card says |
|---|---|
| one address | "Enter this code at **this address**…", then the URL |
| several | "…at **one of these**…", then up to three, then "and N more" |
| only the port | the old "port 8080 on this device" sentence |

`--url` on `shepherd-pairing-display` is now repeatable. A device on wifi and a
VPN has two answers and which one works depends on where the parent's laptop is,
so naming only the first would be a coin flip.

## The 5-second poll, and why it is not 30

The card lives in the same task as the credential store's expiry sweep, which
ran every 30 s. That is far too slow for the card, and the reason is visible in
the log of a cold boot:

```
20:53:37.523  Management API listening addr=0.0.0.0:8080 scheme="https"
20:53:37.544  Showing the management setup code on screen pid=309222
20:53:42.543  Showing the management setup code on screen pid=309539
```

The first card goes up **21 ms after** the listener binds — and still has no
addresses, because the NetworkManager read has not answered yet. On a 30-second
tick a parent would stare at the weaker sentence for half a minute on exactly
the boot where they are trying to set the device up.

So the task polls at 5 s, sweeps on its own 30-second clock, and **only reads
the network while an enrolment code exists** — a device that finished setup
months ago does not make a D-Bus round trip every five seconds for a card that
will never be shown. The card is respawned only when what it would say changes,
so a tick that changes nothing does not flash the overlay at whoever is reading
the code off it. Measured: two spawns, exactly 5.00 s apart, then stable.

## Reverified

### On this machine

`rustc 1.98.1` (the version the previous session's CI-only Clippy failure was
about — check `rustc --version` before believing a "CI-only" lint again).

| | |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace --all-targets` | **1301 passed / 0 failed** |
| `cargo test -p shepherd-e2e -- --include-ignored --test-threads=1` | **19 passed / 0 failed** |
| web UI typecheck / boundary / coverage / vitest | clean, 95 tests |
| `shellcheck` + `check-workflows.sh` as CI runs them | clean |
| companion `assembleDebug` + `testDebugUnitTest` | clean |

New tests: two in `shepherd-api` for the HTTPS scheme (wildcard and concrete
bind), one more assertion each in `shepherd-management`'s listener tests, and
five in `shepherd-pairing-display` — the card's copy decisions were extracted
into `setup_instruction` / `setup_addresses` so they can be tested without a
display.

### On hardware, over BLE

Pixel 10a on USB, daemon pinned to the **Realtek** controller
(`8C:68:8B:41:02:DC`) as the `companion-pairing` skill's two-radios gotcha
requires — a plain `config.example.toml` boot picked the broken Qualcomm one
(`hci1`), which is the first BlueZ lists on this box.

1. **The companion still works.** Reconnected on its existing bond and listed
   all entries. The regenerated wire types (now carrying `tls`) disturbed
   nothing.
2. **The network screen shows the fix.** "Open from a browser at:
   `https://192.168.122.130:8080`" — where before this change it would have said
   `http://`, and the device would have reset the connection.
3. **Web access over BLE** — password card and "No browser is waiting to sign
   in", correct for a store with no password.
4. **`set_web_password` from the phone** — set it, daemon logged the RPC and
   "Web management password set by an authenticated administrator", the new
   password signed in over HTTPS (200) and a wrong one did not (403). The
   on-screen card tore itself down within the next poll.
5. **The approval handshake, including the property that matters.** Two requests
   from two different browsers, both pending:

   ```
   724150  Chrome on macOS · 127.0.0.1
   187636  Edge on Windows · 127.0.0.1
   ```

   Different codes; approving the first returned `approved` with a session for
   **only** that one while the second still returned `pending`; the approved
   cookie answered 200 on `/auth/session`; **Not me** on the survivor turned its
   poll to `denied` and cleared the card from the phone.
6. **Reconnect** after `am force-stop` — came back on the bond, outbox drained
   as expected.

## Things that will bite the next person

- **`cargo check` cannot see a broken rebase resolution in Kotlin.** See above.
  Any rebase that conflicts under `companion-android/` needs a Gradle build
  before you believe it.
- **`ANDROID_HOME=/opt/android-sdk`.** Not `~/Android/Sdk`; nothing in the repo
  sets it.
- **`pgrep -x` cannot match `shepherd-pairing-display`** — the kernel truncates
  `comm` to 15 characters, so it silently matches nothing and an overlay you
  thought you killed is still on screen, on top of the one you just started.
  The bracket trick (`grep "[s]hepherd-pairing-display"`) does not save you
  either: the pattern's own text appears in your `bash -c` command line, so it
  matches and kills your shell (exit 144). Keep the pid from `$!` and `kill`
  that.
- **Two overlays anchored to the same corner stack rather than replace**, and
  the older one wins the screenshot. That is why the card task drops the old
  child before spawning the new one.
- **A backgrounded `cargo test … | tail -N` gives you a log that is only N lines
  long**, so a "passed=" total computed from it is wrong and looks plausible.
  Redirect the whole run to a file and grep that.

## Still open, unchanged from the previous note

- **`docs/INSTALL.md` says nothing about the login screen**, the setup code on
  the TV, the self-signed certificate, or `shepherd web-auth reset`. Left for
  the branch author to place, as before. The card change makes the first-boot
  story better but does not close that gap.
- **Sessions on the companion's Web access screen** (`list_web_sessions` /
  `revoke_web_session` answer over BLE; there is no UI for them).
- **The certificate fingerprint in the companion.**
- **A self-signed certificate still does not name a wildcard bind's addresses**,
  so the URL the card now hands out gets a name mismatch on top of the
  untrusted-issuer warning. Naming a concrete `bind` fixes it completely and is
  the documented answer; see the previous note's item 8 for why enumerating live
  interfaces into the SAN list is not.

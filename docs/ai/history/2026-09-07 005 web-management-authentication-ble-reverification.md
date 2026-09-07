# Web management authentication (issue #156) — rebase, CI fix, and the BLE pass

> Status: **the pause is over.** The branch is rebased onto `main` at `1393cfa`,
> the Clippy failure is fixed, and every item the build handoff listed as
> "NOT verified — needs the BLE adapter and the paired phone" has now been run
> on hardware. Nothing on that list is outstanding.
>
> Build and design history: `2026-09-07 003 web-management-authentication-scope.md`
> and `2026-09-07 004 web-management-authentication-handoff.md`.

## Prompt

> #183 is checked out. Rebase on the current main, fix the CI failure pasted
> below, and reverify including the BLE testing that was previously skipped due
> to missing hardware that is present here
>
> ```
> error: the `Err`-variant returned from this function is very large
>    --> crates/shepherd-http/src/handlers/auth.rs:279:54
>     |
> 279 | async fn split<T: DeserializeOwned>(req: Request) -> Result<(Parts, T), Response> {
>     |                                                      ^^^^ the `Err`-variant is at least 128 bytes
>     = note: `-D clippy::result-large-err` implied by `-D warnings`
> ```

## The rebase

Clean, no conflicts. `main` had moved from `f3cfa9c` (where the handoff left
the branch) to `1393cfa`, picking up the session watchdog packaging fixes and
**#184, "Improve BLE reliability around disconnect events"** — which touches
`crates/shepherd-ble`, the same crate this branch regenerated wire types into.
The BLE pass below therefore doubles as a check that the two changes coexist;
they do.

## The CI failure, and why it did not reproduce locally

`clippy::result_large_err` fired on `split`, whose `Err` variant was a whole
`axum::Response` (128 bytes, at the lint's default threshold).

**It is a toolchain gap, not a flake.** The handoff's `cargo clippy` run was
green because this box was on **1.97.0**; CI's image installs plain `stable`,
which had moved to **1.98.1**, where the lint reaches this function. `rustup
update stable` reproduced it exactly on the first try:

```
error: the `Err`-variant returned from this function is very large
   --> crates/shepherd-http/src/handlers/auth.rs:279:54
```

If a "CI-only" Clippy error turns up again, check `rustc --version` against
the CI image before anything else — `.ci/Dockerfile` pins no version.

**The fix** is not an `#[allow]`. `split` now returns a small `BadBody`
(`TooLarge` / `Malformed(serde_json::Error)`, 16 bytes) with an `IntoResponse`
impl that produces the byte-identical 400 the callers used to receive; the
three call sites became `Err(e) => return e.into_response()`. The `Err` type is
now small for the reason the lint cares about — every caller was paying 128
bytes on the success path too.

Verified on 1.98.1: `cargo clippy --workspace --all-targets -- -D warnings`
clean, `cargo fmt --all -- --check` clean, `cargo test --workspace
--all-targets` **1266 passed / 0 failed**, `cargo test -p shepherd-e2e --
--include-ignored --test-threads=1` **19 passed / 0 failed** (both #156 e2e
tests among them), web UI 90 tests + boundary + coverage + typecheck green,
`shellcheck` and `check-workflows.sh` green, companion `assembleDebug` +
`testDebugUnitTest` green.

## The BLE pass

Hardware: Pixel 10a on USB (`adb`), kernel `7.0.0-31-generic`, and **two**
controllers. shepherdd was pinned to the Realtek one, as the
`companion-pairing` skill's two-radios gotcha requires:

```toml
[service.ble_management]
adapter = "8C:68:8B:41:02:DC"    # Realtek; the Qualcomm DC:56:… is the broken one
```

Everything below was run against a real daemon over a real radio, in the
handoff's numbering.

### 1. The companion still works — yes

The app reconnected to the rebased daemon on its own bond and listed all 17
entries with their reasons. The seven new `ManagementService` methods
regenerated `RpcMethods.kt` / `RpcParams.generated.kt` / `WireTypes.generated.kt`
without disturbing anything already on the wire, on top of #184.

### 2. `web_auth_status` over BLE — yes, and the poll is comfortable

Device controls → **Web access…** showed the password card and "No browser is
waiting to sign in", exactly as predicted.

The handoff flagged the 3-second poll's two round trips as the thing most
likely to strain the 15-second RPC deadline. **Measured, it is not close.** The
two calls land ~200–300 ms apart and the pair completes inside ~500 ms:

```
17:58:33.940 received id=9  web_auth_status
17:58:34.232 received id=10 list_login_requests
17:58:36.961 received id=11 web_auth_status      <- next tick, 3.0 s later
```

No need to fold them into one call.

### 3. The approval handshake — yes, including the property that matters

Two requests were started from two different browsers while both were pending.
They got **different codes**, and the phone showed **two cards**:

```
348263  Chrome on macOS · 127.0.0.1
063297  Edge on Windows · 127.0.0.1
```

Approving the first signed in **only** the first: its poll returned `approved`
with a `Set-Cookie`, and the second poll still returned `pending`. That is the
whole security argument for the design, and it holds on the wire.

- **"Not me"** on the survivor → the browser's next poll returned `denied` and
  the card vanished from the phone.
- **Expiry** → an untouched request returned `pending` at T+45 s and `expired`
  at T+2:09, with the card gone from the phone.
- **Through a real browser**: Firefox over Marionette clicked "Approve on my
  phone", displayed `122646`, the phone showed the same digits labelled
  "Firefox on Linux · 127.0.0.1", and the tap signed the browser in **within
  one second** — the app was rendering the dashboard on the next poll.

### 4. `set_web_password` from the phone — yes

Changed the password from Web access → Change password. The daemon logged
`set_web_password` over BLE and "Web management password set by an
authenticated administrator". Afterwards: the old password → **403**, the new
one → a session, and **the three existing sessions kept working** (`/auth/session`
→ 200), which is the deliberate behaviour ("I changed the password", not "I was
compromised").

### 5. `companion_available` is honest — yes, in both directions, live

- Unclaimed device: `{"configured":true,"companion_available":false}` and the
  login page rendered **`Sign in` only** — no "or", no phone button (checked in
  Firefox's DOM and in a content screenshot, not just in the API).
- After pairing: `companion_available` flipped to `true` **without a daemon
  restart**, and the same page then offered `Sign in | Approve on my phone`.

So the `web.set_companion(admin_authority)` ordering in `shepherdd/src/main.rs`
is right, and the flag reads the claim machine live rather than latching at
startup.

### 6. A factory reset does not strand web auth — yes, and it is the right call

`touch dev-runtime/data/.factory-reset-ble` + restart cleared the admin record
and removed the BlueZ bond (`Removed BlueZ bond peer=…`); the app showed
**"Bond lost — re-pair needed"** and **Re-pair** led back to the scan list.
`web-auth.toml` came through **byte-identical** (same md5), the password still
signed in, and the login page correctly stopped offering the phone.

**This reads as right, not wrong.** The BLE factory reset is about the phone's
claim; the web password is a credential a parent chose, and wiping it on a
BLE-side reset would strand a household whose phone broke — which is precisely
what `shepherd web-auth reset` exists for instead.

One consequence the handoff did not spell out, worth knowing: **browser
sessions minted before the reset stay valid afterwards.** Confirmed by hand —
a cookie from before the reset still answered 200 on `/auth/session` after it.
Reasonable (the reset says nothing about those browsers), but it means "I lost
my phone, factory-reset the device" does **not** sign anyone's browser out.
`auth/sessions/{id}` or `shepherd web-auth reset` is the tool for that. Noted
in <docs/INSTALL.md>'s re-pairing section.

### 7. `list_web_sessions` / `revoke_web_session` over BLE — yes

These have no companion UI, so they were exercised with a **temporary probe**
added to `refreshWebAuth` (log the session list; revoke an id dropped into the
app's own `files/` via `run-as`), installed as a debug build, then reverted —
`git checkout` on `ShepherdViewModel.kt` and a clean reinstall. Nothing of it
is in the branch.

Both answer. `list_web_sessions` returned all four live sessions with correct
labels and `current=false` throughout (right: the companion is not a browser
session), and `revoke_web_session("hZByJpxGmk_1")` removed it from the list and
made its cookie **401** on the next request.

The screen itself is still unfinished — the follow-up stands, but the RPCs
underneath it are proven.

### 8. A self-signed certificate on a real LAN address — the sharp edge is real

Measured on `192.168.122.130`, not reasoned about.

With the example config's wildcard bind, the certificate names only
`localhost, shepherd-26.04.local, shepherd-26.04, 127.0.0.1, ::1` — and a
client reaching the LAN address gets a **name mismatch on top of** the
untrusted-issuer warning:

```
SSL: no alternative certificate subject name matches target ipv4 address '192.168.122.130'
```

**Binding the concrete address fixes it completely**, and is worth preferring
over `files` mode for a device with a reservation:

```toml
[service.management_api]
bind = "192.168.122.130"
```

→ SAN gains `IP Address:192.168.122.130`, and `curl --cacert tls.pem
https://192.168.122.130:8080/…` verifies and returns 200.

**Recommendation: leave `local_addresses` as it is.** Enumerating live
interfaces sounds like the fix until you look at what this box would put in the
certificate — `10.0.3.1` (lxcbr0) and `172.17.0.1` (docker0) alongside the real
address — and it still would not survive a DHCP move without regeneration. The
honest options are a concrete `bind` or `files` mode, and both already exist.
Documenting the concrete-bind trick is worth more than the code change.

### A hostname whose last label is numeric cannot be reached by name over TLS

Found while checking the SANs, and it is not this branch's doing:

```
$ openssl s_client -connect 192.168.122.130:8080 -servername shepherd-26.04
ssl/tls alert illegal parameter ... SSL alert number 47
```

rustls **rejects the SNI** `shepherd-26.04` before the certificate is
considered, because its rightmost label (`04`) is all digits and webpki refuses
such names as possible IPv4 addresses. `localhost` and `shepherd-26.04.local`
against the same listener both return 200, so it is the name, not the setup.

Impact is narrow — it needs a hostname ending in a numeric component, which
this dev box has and a device called `living-room-tv` does not — and there is
no server-side fix: any client sending that SNI is refused. Worth knowing
because the failure looks like a broken TLS config rather than a hostname
problem. `tls.rs` still puts the name in the SANs, which is harmless.

## Smaller observations, none blocking

- **`/api/v1/auth/request` mints a request even when no companion is
  available.** The UI never gets there (the button is hidden off
  `companion_available`), and it cannot produce a session, but a direct caller
  gets a code that nobody can approve and that expires in two minutes. It is
  charged against the peer's throttle like a failed login, so it is not a way
  to fill the pending list. Cosmetic; a `CompanionUnavailable` error would be
  tidier.
- **The Web access screen's refresh spinner never rests**, because the
  3-second poll sets `loading = true` on every tick. Cosmetic.
- **The branch adds no user-facing documentation.** `docs/INSTALL.md` says
  nothing about the login screen, the setup code on the TV, the self-signed
  certificate, or `shepherd web-auth reset` — a parent installing this would
  meet a login page the install guide has never mentioned. Only the
  factory-reset interaction was added here (it is what item 6 asked about); the
  rest is a real gap and deliberately left for the branch author to place.

## Things that will bite the next person

- **`dev headless` can fail with "shepherdd did not connect to the compositor
  within 30s" while the stack is perfectly healthy.** A
  `Cannot get portal org.freedesktop.host.portal.Registry version: Timeout was
  reached` in GTK costs ~26 s of the 30 s budget; the alias socket appeared at
  T+50 s and everything worked. But the script had already bailed **without
  writing `session.env`**, so `dev stop` then denies there is a session and
  leaves the whole stack orphaned. Symptom: `ls /run/user/1000/` shows the
  alias, `dev-runtime/shepherd.sock` is live, and no `dev` subcommand can
  reattach. Kill the pids from `ps -eo pid,cmd | grep -E "sway.*headless|shepherdd"`,
  remove `session.env`, boot again.
- **A curl cookie jar is keyed by host.** Moving the daemon from a loopback
  bind to a LAN bind made every saved session look revoked (401) because curl
  silently stopped sending the cookie. Send it with
  `-H "Cookie: shepherd_session=…"` before concluding anything about session
  lifetime.
- **`uiautomator dump` drops some Compose `Text` nodes.** The Web access
  screen's "No browser is waiting to sign in" is in the pixels but not in the
  dump. Screenshot before believing a label is missing.

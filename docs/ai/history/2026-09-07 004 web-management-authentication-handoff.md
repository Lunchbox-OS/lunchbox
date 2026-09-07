# Web management authentication (issue #156) — build handoff

> Status: **built and verified as far as this environment reaches.** Everything
> that does not need a Bluetooth adapter or a paired phone is done, tested, and
> seen working end-to-end in the headless dev session against real TLS. The
> companion half compiles and its unit tests pass, but **no part of it has been
> run against a device**, because this machine has no BLE adapter and no phone.
> The list of what to test with one is at the bottom, in the order to do it.
>
> Design and decisions: `2026-09-07 003 web-management-authentication-scope.md`.
> All five decisions were followed as recorded; the two places the build
> departed from the plan are under "As built, versus the scope".

## Prompt

> build it, but pause and mark down your work and what needs to be tested in a
> note so I can pick it up in an environment that has a BLE adapter and paired
> phone

## What landed

Four pieces, in the build order the scope proposed. Nothing is behind a flag:
the change is live the moment the daemon starts.

### 1. Sessions, and the middleware that reads them

- **`crates/shepherd-management/src/webauth.rs`** (new, ~700 lines with tests).
  The credential store: Argon2id password, sessions (hashed at rest, idle *and*
  absolute expiry, revocable), pending companion-approval requests (in memory
  only), and the per-peer login throttle with doubling backoff. 34 unit tests.
- **`crates/shepherd-http/src/auth.rs`** (rewritten). Resolves a session cookie,
  then a bearer token — first as a session, then as a machine credential —
  into an `Identity` in the request extensions. Adds the `Origin`-vs-`Host`
  check on cookie-authenticated writes.
- **`ProtectedFile::WebAuth`** (`web-auth.toml`) and **`ProtectedFile::TlsCert`**
  (`tls.pem`), both `System` scope, both added to `scripts/lib/install.sh`'s
  `SHEPHERD_SYSTEM_FILES` — the four-place change the drift test enforces.
- **The static `auth_token` is demoted, not removed**, per decision 4: it
  authenticates a request and cannot open a browser session. The e2e harness now
  always configures one (`DEFAULT_E2E_AUTH_TOKEN`), because a harness with no
  credential would be driving a device that refuses every RPC.

### 2. TLS

- **`crates/shepherd-http/src/tls.rs`** (new). `files` mode reads a PEM pair;
  `self_signed` generates one with rcgen for the device's names and addresses,
  persists it through the custodian, and logs its SHA-256 fingerprint.
- **`axum-server` + `rustls` on the `ring` provider**, chosen because rustls is
  already in the tree behind `ureq` on `ring` — so this adds no `aws-lc-rs` and
  no cmake to a device build.
- **`[service.management_api.tls].mode`**: `auto` (the default; plaintext on
  loopback, self-signed anywhere else), `off`, `self_signed`, `files`.
  **`off` with a non-loopback bind is a config-validation error.** Because the
  self-signed fallback always exists, that hard stop can never lock a device
  out of its own management UI — the objection that ruled out BYO-only.

### 3. Password, enrolment, and the web UI

- **Five pre-auth endpoints** (`status`, `setup`, `login`, `request`, `poll`)
  and four authenticated ones (`session`, `signout`, `sessions`,
  `sessions/{id}`), in `crates/shepherd-http/src/handlers/auth.rs`.
- **The first-run enrolment code** is generated when the store has no password,
  persisted so a restart mid-typing does not invalidate it, logged at `WARN`,
  and **shown on the device's own screen** as a corner card —
  `shepherd-pairing-display --setup-code`, a second mode alongside the
  full-screen pairing overlay. The card is torn down within 30 seconds of a
  password existing.
- **The SPA**: `AuthGate` decides between the login page and the app on load and
  on any later 401; `LoginPage` has all three shapes (setup / password /
  approve-on-phone); `SessionsCard` lists and revokes. The token field in
  `ConnectionSettings` survives, reworded as a machine token for the
  cross-origin case.
- **`shepherd web-auth reset`** (`scripts/lib/webauth.sh`) is the SSH last
  resort: remove the store, get a fresh code on screen.

### 4. Companion approval

- **Seven new `ManagementService` methods**, so codegen carried them to BLE, to
  Kotlin, and to TypeScript for free: `web_auth_status`, `set_web_password`,
  `list_web_sessions`, `revoke_web_session`, `list_login_requests`,
  `approve_login_request`, `deny_login_request`.
- **`ui/webauth/WebAccessScreen.kt`** (new) plus `WebAuthUiState` and four
  methods on `ShepherdViewModel`. Reached from Device controls → "Web access…".
- The split the scope insisted on holds: `login/request` and `login/poll` are
  HTTP-only pre-auth endpoints, and the *approval* is an ordinary authenticated
  RPC. BLE never sees a login.

## Verified here

Everything below was actually run, not reasoned about.

| What | How |
|---|---|
| Store behaviour: hashing, both expiries, revocation, lockout arithmetic, the approval handshake, "two racing requests get different codes" | 34 unit tests, `cargo test -p shepherd-management` |
| Route reachability, cookie attributes (`HttpOnly`/`SameSite=Strict`/`Secure`), the `Origin` check both ways, 429 + `Retry-After`, the whole approval handshake over HTTP | 19 integration tests in `crates/shepherd-http/tests/api.rs` |
| TLS mode resolution and the plaintext hard stop | 11 tests in `crates/shepherd-config/src/validation.rs` |
| The login page's three shapes, and that the phone button appears only when a companion is paired | 7 jsdom tests, `shepherd-webui/src/auth/login.test.tsx` |
| **A real daemon**: self-signed cert generated, `https` listener up, setup code on disk matching the one the API accepts, cookie issued, RPC authenticated, sign-out ending the session for real | headless dev session + `curl -k`; and the two `#[ignore]` e2e tests below |
| **A real browser**: the setup screen, completing setup through the form, the session card showing "Firefox on Linux · this browser", sign-out bouncing back to the login page | Firefox over Marionette in the headless session, screenshots taken |
| **The on-screen setup card**, rendered over a live launcher and torn down once a password exists | `dev shot` |

Two new e2e tests, `#[ignore]`d like the rest of that suite and **run here**:
`the_setup_code_on_disk_logs_a_browser_in` and
`a_password_login_works_and_a_wrong_one_does_not`. The whole e2e suite —
`cargo test -p shepherd-e2e -- --include-ignored --test-threads=1`, exactly as
CI runs it — is green, so the change does not disturb the firewall, browser,
ebook, retroarch or IPC-peer paths either.

Full suite: **1263 passed, 0 failed**, on top of `main` at `f3cfa9c` — the
branch was rebased onto the session watchdog (#172) and the packaging fixes
(#177) and re-verified there, browser flow included. `cargo clippy --workspace --all-targets`
clean, `cargo fmt --all` applied, `shellcheck` clean the way CI runs it, web UI
typecheck / boundary / coverage / 90 tests all green, companion
`compileDebugKotlin` and `testDebugUnitTest` green.

## NOT verified — needs the BLE adapter and the paired phone

This is the whole reason for the pause. Nothing below has run even once.

### In order

1. **The companion still works at all.** Before touching anything new: pair (or
   reconnect) a phone and confirm the existing screens load. Seven methods were
   added to `ManagementService`, which regenerated `RpcMethods.kt`,
   `RpcParams.generated.kt` and `WireTypes.generated.kt`. Those are mechanical,
   but a wire regression would look like "the app is broken" rather than like
   this change. Use the `companion-pairing` skill.

2. **`web_auth_status` over BLE.** Open Device controls → **Web access…**. It
   should show the password card and "No browser is waiting to sign in". A
   failure here is the RPC plumbing, not the UI.
   - *Watch for*: the 15-second RPC deadline. `list_login_requests` and
     `web_auth_status` are two round trips on every 3-second poll — the fastest
     poll in the app. If BLE throughput makes that unhappy, the fix is to fold
     them into one call, not to slow the poll: a parent is waiting on it.

3. **The approval handshake, end to end.** Device has a password. On a laptop,
   open the device's page, choose **Approve on my phone**. Six digits appear.
   The phone should show the same six digits within ~3 seconds, with a label
   like "Chrome on macOS" and the laptop's IP. Tap **Approve** → the laptop
   should be signed in within ~2 seconds.
   - *The property to actually check*: start a second request from a **different
     browser** while the first is pending. Two cards, **different numbers**.
     Approving one must sign in only that one. This is the entire security
     argument for the design and the only test that exercises it.
   - Also check **Not me** (the browser should say the request was declined) and
     letting a request **expire** (2 minutes; the browser should say so and the
     card should vanish from the phone).

4. **`set_web_password` from the phone.** Web access → Change password. Then
   sign in on a laptop with the new one. Existing sessions should *survive*
   this — that is deliberate ("I changed the password", not "I was
   compromised"). Confirm the old password stops working.

5. **`companion_available` is honest.** On a device with **no** phone paired,
   the web login page must **not** offer "Approve on my phone". Then pair a
   phone and reload: it should appear. This reads the BLE claim machine through
   `WebAuth::set_companion`, which is wired in `shepherdd/src/main.rs` after the
   BLE server is constructed — the one ordering that could silently be wrong.

6. **A factory reset does not strand web auth.** BLE factory reset clears the
   admin record and the bond. It deliberately does **not** touch
   `web-auth.toml`: the web password is not the phone's. Confirm that after a
   reset the web UI still signs in with its password, and that the login page
   stops offering the phone. If that reads as wrong to you, it is a decision to
   revisit rather than a bug to fix — say so and I will change it.

7. **`list_web_sessions` / `revoke_web_session` over BLE.** These are
   implemented and reachable but have **no UI on the phone** — the companion
   screen shows the password card and pending requests only. Either confirm
   they answer (a manual RPC) or treat the screen as unfinished; see Follow-ups.

### Also worth a device, though not BLE

8. **A self-signed certificate on a real LAN address.** Everything here was
   loopback. On a device bound `0.0.0.0`, the SANs come from `/etc/hostname`
   plus the *bind* address — and a wildcard bind contributes no address at all,
   so a phone reaching `https://192.168.1.x:8080` will get a **name mismatch**
   on top of the untrusted-issuer warning. That is a known sharp edge (see
   `local_addresses` in `crates/shepherd-http/src/lib.rs`) and the reason
   `files` mode exists. Decide whether it is acceptable or whether the
   certificate should enumerate the machine's live interfaces.

9. **`tailscale cert` into `files` mode**, if you want the padlock. Untested.

## As built, versus the scope

Two departures, both deliberate:

- **The scope said the SPA's sessions page would use the HTTP auth endpoints so
  it could mark `current`.** It does. The consequence is that
  `list_web_sessions` and `revoke_web_session` on the trait exist for the
  *companion*, and the companion has no UI for them yet. Not a bug, but the
  scope implied both would be finished together.
- **`stored_fingerprint` was written and then removed.** `tls.rs` already logs
  the fingerprint at exactly the right moment in both paths (generation and
  load), so the shepherdd-side call would have logged the *previous* boot's
  value on a first run. Showing it in the companion — which is what would make
  the click-through verifiable for a parent rather than for whoever reads the
  journal — needs a wire type change and is a follow-up.

## Follow-ups, none blocking

- **A diagnostic for "management API reachable off-device without TLS"** and one
  for "no password set". Listed in the scope, not built: it needs a
  `DiagnosticCode` variant, and the config validation error already makes the
  first state unreachable.
- **The certificate fingerprint in the companion**, per above.
- **Sessions on the companion screen** (item 7).
- **A `--port` for the setup card is a stopgap.** A device on DHCP that moves
  gets a stale certificate and a card that names no address. mDNS would fix
  both and is out of scope here.
- **The dev proxy now sets `changeOrigin: false`** (`rsbuild.config.ts`).
  With `changeOrigin: true` the daemon's CSRF check sees `Host: localhost:8080`
  against `Origin: http://localhost:3000` and answers 403 on every write. Worth
  knowing before anyone "fixes" that line back.

## Things that will bite the next person

- **`http://127.0.0.1:8080` is gone from the dev loop.** `config.example.toml`
  binds `0.0.0.0`, so the dev stack is **https** with a self-signed cert.
  Marionette needs `acceptInsecureCerts` at `WebDriver:NewSession` *and* a
  launch on `about:blank` (a URL on the command line loads before capabilities
  apply). The `headless-dev` skill has been updated with both.
- **`pkill -f firefox` kills your own shell** in this harness — the Bash tool's
  wrapper carries the command text, so `-f` matches it. Cost me a session. Also
  now in the skill.
- **Seeding `localStorage.apiToken` no longer signs a browser in.** Complete
  setup instead: the code is in `dev-runtime/data/web-auth.toml`, and
  `rm`-ing that file plus a restart gets you back to a fresh device.
- **`target/debug/incremental` reached 26 GB and filled the disk** mid-run,
  which surfaced as linker "Bus error". Check `df` when a build dies oddly.

  > **Correction (2026-09-07, session 006).** The one test failure recorded here
  > as collateral of that — `retroarch_spawn_materializes_config_and_argv` — is
  > **not** a disk-space symptom. It is a genuine race on a process-global
  > environment variable, and it reproduces on a box with plenty of free space.
  > `adapter.rs`'s test sets `RETROARCH_ROOT_ENV` and `retroarch.rs`'s tests
  > `remove_var` the same variable, on parallel threads of the same test binary;
  > when a removal lands between the set and the read, the code falls back to the
  > real `~/.local/share/shepherdd` and the argv assertion fails with a home-directory
  > path on the left and the tempdir on the right. That left/right pair is the
  > tell. It passes in isolation and on rerun, so it still reads as a flake.
  > `sway_ipc.rs`'s `ENV_LOCK` is the pattern that fixes it; the two modules would
  > need to share one lock. Not yet done.

# Web management authentication (issue #156) — scope

> Status: **scoped, not built.** This is the survey and the argument, not an
> implementation. The five judgement calls it turned on were answered on
> 2026-09-07 and are recorded under "Decisions" at the end; the body above
> them is the reasoning that produced the options, kept as written so a later
> reader can see what was rejected and why.

## Prompt

> scope out #156

[Issue #156](https://git.armeafamily.com/albert/shepherd-launcher/issues/156),
*"Web management authentication"*:

> There's currently a bearer token here, and not much else.
>
> We need a proper login screen, with a default password and reset flow, etc.
> For now, the reset flow can just be over SSH.
>
> Alternatively, if the companion is set up, we can use that as the
> authentication mechanism -- request log in, then confirm in the
> (previously-paired) app.
>
> Either approach requires HTTPS to prevent someone sniffing the token that
> results from this authentication.

No comments, no labels.

## What exists today

All of it, so the size of the change is not guessed at.

**The gate** is 103 lines of Axum middleware
(`crates/shepherd-http/src/auth.rs`). It reads `Authorization: Bearer <t>` and
compares `t` against two sources held in `AuthSources`:

- `static_token` — `[service.management_api].auth_token` from `config.toml`.
- `admin` — an `Arc<dyn AdminAuthority>`
  (`crates/shepherd-management/src/auth.rs:19`), whose only method is
  `current_http_token() -> Option<String>`. In production the implementor is
  `ClaimMachine` (`crates/shepherd-ble/src/claim.rs:203`), returning
  `AdminRecord::http_token` — 32 random bytes minted at BLE claim time
  (`crates/shepherd-ble/src/admin.rs:44`) and persisted in `admin.toml` through
  the state custodian.

If **neither** source is populated the middleware returns `next.run(req)`
unconditionally: the API is wide open. That is deliberate and documented — a
BLE-enabled device that nobody has claimed yet would otherwise lock itself out
of HTTP — but it means *"unclaimed"* and *"no `auth_token`"* together equal
*"full admin to anyone who can reach the port"*.

**The router** (`crates/shepherd-http/src/handlers/mod.rs:27`) puts the
middleware on the `/api/v1` nest only. Everything else — the whole SPA, from
`index.html` down — is served by the `rust_embed` fallback
(`web_assets.rs:30`) with no auth at all. That is unavoidable for a login page
and fine as it stands, but it does mean the attacker gets the login screen for
free.

**The surface behind the gate** is two endpoints: `POST /api/v1/rpc`,
dispatching into all 45 `ManagementService` methods (`docs/rpc-schema.json`),
and `GET /api/v1/events`, the full `shepherd_api::Event` firehose. Behind that
gate is every administrative action there is: `launch`, `adjust_tokens`,
`upsert_override`, `reload_config`, `set_screen_power`, `act_on_window`.

**How the token reaches a browser today**: by hand. `ConnectionSettings.tsx`
has a password-typed `TextField` labelled "Auth Token (optional)" whose value
goes into `localStorage.apiToken`, read back by an axios request interceptor
(`shepherd-webui/src/api/client.ts:28`). There is no flow that *gives* the
parent the token — the only copies are in `config.toml`, in the custodian's
`admin.toml` (readable over SSH as root, by design unreadable to any activity),
and in the companion app's `AdminRecordStore`, which holds it in
Keystore-backed encrypted storage and **never uses it**: the companion speaks
BLE exclusively. So the realistic answer to "where does the parent get the
string they type into the phone" is *ssh in and cat a TOML file*.

**What is not there at all**: TLS, sessions, expiry, idle timeout, revocation
short of factory reset, rate limiting, lockout, an audit trail of who logged in
from where, and any notion of a password.

## Who the adversary is

Worth stating plainly, because it decides several arguments below.

The threat model for this project is not a stranger on the internet. It is **a
motivated child on the same LAN, with physical access to the device and time**.
That changes the usual priorities:

- *Passive sniffing is realistic.* The child has a laptop on the same Wi-Fi. A
  bearer token that crosses the wire in cleartext, once, is compromised
  permanently — there is no expiry and no rotation.
- *The login page is reachable by the adversary*, from the device's own browser
  activity as well as from the LAN, so an online guessing attack against a
  password is a real attack and rate limiting is not optional.
- *The device's own screen is an untrusted display for approvals.* The
  `shepherd-pairing-display` overlay is exactly the right channel for
  first-boot enrolment, when the parent is holding the device, and exactly the
  wrong one for "approve this login", when the child is sitting in front of it.
- *Conversely, an active on-path MITM is not the likely attack.* The child can
  ARP-spoof, but the far cheaper attack is watching a plaintext header go by.
  This is why a self-signed certificate — worthless against an attacker who was
  present at first contact, effective against everything else — is more useful
  here than its reputation suggests.

## Six things wrong with the status quo

1. **The bearer token is simultaneously the password and the session.** There
   is no exchange step, so the long-lived secret travels on every request.
2. **It never expires and cannot be revoked** except by BLE factory reset,
   which also drops the phone's bond — a wildly disproportionate response to
   "my laptop was stolen".
3. **It is a 32-byte base64 blob typed by hand on a phone.** Whatever the
   security merits, this is the part a parent will actually hate.
4. **It crosses the network in the clear.** `config.example.toml:200` ships
   `bind = "0.0.0.0"`, so the shipped example is a plaintext admin credential
   on the LAN.
5. **It fails open.** No static token plus no claim equals no authentication,
   silently, with nothing in `list_diagnostics` to say so.
6. **`localStorage` is the wrong store for it.** Any script that runs in the
   SPA's origin can read it out; it also survives forever with no server-side
   record that the session exists.

## The shape of a fix

Three layers, separable, and worth keeping separate in review. Layer 1 is the
one that makes the others possible; layer 2 is what the issue calls the login
screen; layer 3 is the HTTPS the issue says both approaches need.

### Layer 1 — sessions, not a naked secret

Introduce a session table and stop treating the credential as the
authenticator.

- `POST /api/v1/auth/login` takes a credential (layer 2 decides which) and
  answers with a session: 32 random bytes, plus `issued_at`, `expires_at`,
  `last_seen`, and a human label derived from the User-Agent so the sessions
  list reads "Pixel 10a · Chrome" rather than a hex string.
- `GET /api/v1/auth/session` is whoami; `POST /api/v1/auth/signout` revokes the
  current one. These three live **outside** `require_auth` (except whoami and
  signout, which need it) and so need an exemption in the router's nest.
- **Naming trap**: `ManagementService` already has a `logout` method, and it
  means *log the kiosk child out of the device session*. The new endpoints must
  not be called `logout` anywhere — `signout`, or `auth.*`, or both.
- Idle timeout and absolute lifetime, both configurable, both defaulting to
  something a parent will not fight: idle 14 days, absolute 90.
- A sessions page in the admin UI listing every live session with its label and
  last-seen, and a revoke button per row plus "revoke everything". This is the
  answer to point 2 above and it is cheap once the table exists.
- The BLE-minted `http_token` keeps working as a credential — it is how the
  companion would ever use HTTP, and how `curl`/e2e drive the API — but it
  becomes a way to *obtain* a session rather than a permanent skeleton key. See
  the decision on the static token below.

**Where the table lives.** In memory, with a lazy write-through to a new
`ProtectedFile::WebAuth` under the custodian. Not a write per request:
`ProtectedFiles::write` replaces the whole file
(`crates/shepherd-util/src/paths.rs:329`), and on a device that is a round trip
to the custodian socket, so `last_seen` updates must be debounced or the
management API becomes a write amplifier. Sessions surviving a daemon restart
is worth the file; `last_seen` being a few minutes stale is not worth a write.

**Adding that variant is a four-place change**, and the test enforces it:
`ProtectedFile`, `scope()` (this one is `System` — it is a fact about the
device, like the admin record), `file_name()`, and the shell list in
`scripts/lib/install.sh`, checked by
`crates/shepherd-util/tests/installer_covers_protected_files.rs`.

**Cookie or bearer.** Both, resolving to the same table:

- Browsers get `HttpOnly; SameSite=Strict; Path=/` — unreadable to script,
  which is the point of moving off `localStorage`, and not sent on any
  cross-site request, which is most of the CSRF answer. Add `Secure` the moment
  layer 3 lands; it cannot go on a plaintext non-loopback origin, which is one
  more reason the two layers ship together.
- Programmatic clients (e2e, `curl`, a future companion-over-HTTP) keep
  `Authorization: Bearer`.
- CSRF, beyond `SameSite=Strict`: `/rpc` requires `Content-Type:
  application/json`, which a cross-origin HTML form cannot set without a
  preflight, and an `Origin` check on state-changing requests is a few lines.
- **Cost**: cookies are same-origin, and `ConnectionSettings` lets the SPA be
  pointed at a *different* device's `apiBase`. That field survives for bearer
  users and for `npm run dev` against a device, but the cookie path only works
  when the SPA was served by the daemon it is talking to — which is the normal
  case, since the daemon serves the SPA. Worth saying out loud rather than
  discovering in review.

### Layer 2 — the credential

Three candidate mechanisms. The issue names two; the third is here to be
rejected on the record.

**A. Password.** Argon2id (`argon2` crate — not currently a workspace
dependency), hash in the custodian's file, never in `config.toml`. Needs, and
this is most of the work:

- Rate limiting with lockout backoff, per source address and globally. The
  adversary can reach the login endpoint; without this, a password is theatre.
- A reset path. The issue says SSH is fine for now, which means a documented
  `shepherd admin reset-password` subcommand or a sentinel file alongside
  `.factory-reset-ble` — the sentinel pattern already exists
  (`ProtectedFile::ResetSentinel`) and is the cheaper of the two.
- A bootstrap story, which is a decision, not a detail. See below.

**B. Companion approval.** The parent opens the web UI, which shows a 6-digit
code and blocks; the paired phone — already bonded, already the admin, already
talking over an encrypted-authenticated GATT link — shows the pending request
and the same code; the parent compares and taps approve; the browser gets a
session.

This is the better mechanism on almost every axis. No shared secret ever
crosses the LAN in either direction, plaintext or not. It reuses the exact
numeric-comparison ritual the parent already performed during pairing, so there
is nothing new to teach. It gives the password reset flow a home that is not
SSH. And it fits the existing architecture unusually well: the approval side is
an ordinary `ManagementService` method, so `#[management_rpc]` carries it to
the BLE transport and the Kotlin/TypeScript codegen for free.

The split matters and is worth getting right the first time: `login/request`
and `login/poll` are **HTTP-only, pre-auth endpoints** — the BLE transport has
no use for them and must not expose them — while `list_login_requests` and
`approve_login_request` are **RPC methods**, post-auth, called by the companion
over BLE.

Its costs are real: companion app work (a new screen, in a separate Kotlin
codebase), a dependency on the phone being in BLE range, and it does nothing at
all for a device that was never claimed. It cannot be the only mechanism.

**C. Approve on the device's own screen.** Rejected: the child is in front of
that screen. The `shepherd-pairing-display` overlay stays in scope for exactly
one thing — showing a one-time enrolment code at first boot, when the parent
has the device in hand.

**Recommendation: build A and B, with B as the path a claimed device
advertises.** Password is the floor, because an unclaimed device and a lost
phone both need it to exist. Companion approval is what a set-up device should
actually offer, and it retires SSH as the only reset path.

### Layer 3 — HTTPS

The genuinely hard layer, because a LAN device with a rotating IP and no public
name is the case browser PKI handles worst. Four options:

1. **Bring your own cert.** `tls = { cert = "...", key = "..." }`, and
   `shepherd-http` terminates with `axum-server` + `rustls`. This is not a
   compromise choice — it is the *superset*: it covers `tailscale cert` (which
   hands out genuinely browser-trusted certs for `*.ts.net` with no CA install
   and no DNS work, and is by a distance the best experience available today),
   Let's Encrypt via DNS-01 for a family that owns a domain, and a home CA for
   someone who already runs one. Small, honest, and unlocks every good answer
   without shepherd needing to know about any of them.
2. **Generate a self-signed cert** (`rcgen`), persisted, with SANs for the
   device hostname and its current addresses. Browsers show an interstitial the
   first time on each device. That is worse than it sounds *and* better than it
   sounds: worse, because teaching a parent to click through certificate
   warnings is a bad habit to install; better, because the fingerprint can be
   shown in the companion app and in the installer output, so the click-through
   is verifiable rather than blind, and because it defeats the passive
   sniffing that is the actual threat here.
3. **A device-generated local CA** the parent installs on each of their
   devices. Real padlock, no warnings — but Android has spent a decade making
   user-installed roots deliberately painful (trusted by Chrome for browsing,
   ignored by apps by default since Android 7), and it is per-parent-device
   setup work forever. Not worth it for a two-parent household.
4. **Don't do TLS; require a private network.** Bind to loopback or a
   ZeroTier/Tailscale interface only — `bind_retry_seconds` exists precisely
   because that interface comes up late — and let the overlay network provide
   the encryption. Defensible, and it is what the current deployment leans on,
   but it makes "open the web UI from your phone on the couch" a VPN setup
   problem.

**Recommendation: 1 and 2, config-selected, plus a hard stop.** `tls = "off"`
stays for loopback and dev; anything else with a non-loopback bind is a
config-validation error rather than a warning. That single rule also kills
failure 5 from the list above: fail-open becomes unrepresentable, instead of
being a state a fresh install happens to sit in.

An honest alternative is to split layer 3 into its own issue and land 1+2
first. The issue asserts both approaches need HTTPS, which is true of the
password (it crosses the wire) but *not* strictly true of companion approval
(nothing secret crosses the LAN — the session token still does, though, so the
plaintext window is one hijackable cookie wide). That is a decision, below.

## What this touches

Roughly in dependency order. Sizes are gut estimates for a reviewer, not
commitments.

| Where | What | Size |
|---|---|---|
| `crates/shepherd-util/src/paths.rs` + `scripts/lib/install.sh` | `ProtectedFile::WebAuth`, `System` scope, installer list | XS, but four places |
| `crates/shepherd-management/src/auth.rs` | Sessions, password hash, verification, rate limiter — `AdminAuthority` grows or is joined by a sibling trait | M |
| `crates/shepherd-http/src/auth.rs` | Middleware reads cookie *or* bearer, resolves through the session table | M |
| `crates/shepherd-http/src/handlers/auth.rs` (new) | `login`, `login/request`, `login/poll`, `session`, `signout` | M |
| `crates/shepherd-http/src/handlers/mod.rs` | Route nest, pre-auth exemptions, `Origin` check | S |
| `crates/shepherd-http/src/lib.rs` | rustls termination, cert loading, self-signed generation | M |
| `crates/shepherd-config` (`policy.rs`, `schema.rs`) | `[service.management_api].tls`, session lifetimes, lockout knobs; the plaintext-plus-public-bind validation error | S |
| `ManagementService` trait | `list_login_requests`, `approve_login_request`, `list_sessions`, `revoke_session` — then regenerate: `cargo run -p shepherd-management --bin rpc-codegen` | S |
| `shepherd-webui` | Login page, first-run setup page, "waiting for approval" page, sessions list; delete the token field from `ConnectionSettings`; 401 handling that routes to login instead of failing the call | L |
| `companion-android` | Pending-approval screen, approve/deny, and the cert fingerprint display if option 2 lands | M, separate codebase |
| `crates/shepherd-e2e` | `auth_token` builder path either survives as a bearer credential or is replaced by a login step in `HttpClient` | S–M |
| `crates/shepherd-api/src/diagnostics.rs` | A diagnostic for "management API is reachable off-device without TLS" and one for "no credential configured" | XS |
| `config.example.toml`, `docs/INSTALL.md`, `crates/shepherd-http/README.md`, `.claude/skills/headless-dev/SKILL.md` | The dev-token instructions in the skill go stale the moment this lands | S |

## Testing

- Unit, in `shepherd-management`: hash/verify, session expiry (idle and
  absolute), revocation, lockout counters and their reset.
- Integration, extending `crates/shepherd-http/tests/api.rs`: pre-auth
  endpoints reachable without a credential and nothing else is; a cookie
  session authenticates; a revoked session 401s; an expired one 401s; lockout
  engages and releases; a cross-origin `Origin` is refused on `/rpc`.
- e2e, in `crates/shepherd-e2e`: a real login against a real daemon, then a
  real RPC, then a signout that actually stops working. The existing
  `auth_token_is_required_when_configured` test is the template and the thing
  most likely to break.
- Browser, through the headless dev session: the login page, a wrong password,
  the lockout message, and the first-run setup flow, all captured with `dev
  shot`. This is a UI change and the project's rule is that UI changes are seen
  before they are claimed.
- Companion approval end-to-end needs the `companion-pairing` skill and a real
  phone; no unit test reaches it.

## Explicitly not in scope

- Multi-admin and roles. `AdminRole` has one variant and the BLE design defers
  this to v2; nothing here should quietly become the multi-user story.
- Anything about the child's own authentication. `logout` and the kiosk session
  are a different subject that happens to share a word.
- Remote/internet access to the management API. Everything above assumes LAN or
  overlay network.
- Hardware-button factory reset, still the right long-term answer to lockout
  and still not this issue.

## Decisions

Answered 2026-09-07. Every one landed on the recommendation, so the argument
for each is the section it came from; only the consequences are repeated here.

1. **Mechanisms — both, password as the floor.** A device that was never
   claimed, and a parent whose phone is gone, both need a path that does not
   involve a phone. Companion approval is what a claimed device offers, and
   its existence is what lets the password reset flow stop being SSH.
2. **Bootstrap — first-run enrolment code, no shipped default.** At first boot
   the device shows a one-time code through `shepherd-pairing-display`; the
   first browser to present it sets the password. This is the one moment the
   device's own screen is a trusted channel, because the parent is holding it
   and the child has not met it yet. It also means there is no credential in
   the repository, the docs, or a factory image.
3. **HTTPS — in this issue, both certificate sources.** BYO cert
   (`tailscale cert`, Let's Encrypt DNS-01, a home CA) plus a generated,
   persisted self-signed fallback with SANs for the device's names and
   addresses, whose fingerprint the companion app and the installer output
   both display so the interstitial click-through is verifiable rather than
   blind. Plus the hard stop: plaintext with a non-loopback bind is a
   config-validation error. Because the self-signed fallback always exists,
   that stop can never lock a device out of its own management UI — which is
   the objection that ruled out BYO-only.
4. **Static `auth_token` — kept, demoted to a bearer-only credential.** It may
   authenticate an `Authorization: Bearer` request and may not open a browser
   session, and it is documented as a machine credential for e2e and the
   headless dev loop rather than as a way for a person to log in. This finally
   closes the question the June BLE design left open
   (`docs/ai/history/2026-06-20 002 ble-management.md`, "Open questions").
   e2e's `auth_token` builder path survives unchanged; the login flow gets its
   own e2e coverage rather than replacing that one.
5. **Session storage — cookie for browsers, bearer for programmatic clients,
   one session table behind both.** `HttpOnly; SameSite=Strict; Secure`, the
   last of which layer 3 makes possible. The accepted cost: the cookie path
   requires that the daemon served the SPA, so `ConnectionSettings`'
   cross-origin `apiBase` field becomes a bearer-only affordance for
   `npm run dev` and cross-device use, and should say so in its helper text
   rather than silently half-working.

## Build order

Given the above, four independently reviewable pieces:

1. **Session table and the middleware that reads it.** `ProtectedFile::WebAuth`
   (four places, see above), the table in `shepherd-management`, the
   middleware resolving cookie-or-bearer, the static token demoted to
   bearer-only. No new UI; every existing client keeps working, which is what
   makes this reviewable on its own.
2. **TLS.** rustls termination, the two certificate sources, the config knobs,
   the validation error, the fingerprint surfaced. Lands before any password
   crosses the wire, so `Secure` is available to the next step rather than
   retrofitted.
3. **Password, enrolment, and the login UI.** Argon2id, rate limiting with
   lockout, the first-run enrolment code through the pairing display, the SPA's
   login and setup pages, the sessions list, the SSH reset sentinel, and the
   401-routes-to-login handling. The largest of the four.
4. **Companion approval.** `list_login_requests` / `approve_login_request` on
   the trait (codegen carries them to BLE and both client languages), the
   HTTP-only `login/request` and `login/poll` pre-auth endpoints, the browser's
   waiting screen, and the companion's approval screen. Verified with the
   `companion-pairing` skill against a real phone, because nothing else
   reaches it.

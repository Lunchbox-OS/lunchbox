//! Web management authentication: passwords, sessions, and the
//! companion-approval handshake (issue #156).
//!
//! What this replaces. The management API used to authenticate by comparing
//! `Authorization: Bearer` against a long-lived secret — either the static
//! `auth_token` from the config file or the token the BLE claim flow minted.
//! That secret was the password and the session at once: it crossed the wire on
//! every request, never expired, and could not be revoked without a factory
//! reset that also dropped the phone's bond. This module is the exchange step
//! that was missing. A *credential* — a password, or an approval tapped on the
//! paired companion — buys a *session*, and it is the session that travels.
//!
//! Three things live here, and they are here rather than in `shepherd-http`
//! because the companion reaches two of them over BLE:
//!
//! 1. **The password.** Argon2id, hashed into `web-auth.toml` under the state
//!    custodian. Never in `config.toml`: a policy file is readable by the
//!    people a policy is *about*.
//! 2. **Sessions.** Minted on login, hashed at rest, expiring on both an idle
//!    and an absolute clock, revocable one at a time or all at once.
//! 3. **Login requests.** The browser asks, the phone approves. Deliberately
//!    in memory only — a pending request is a thing a human is looking at
//!    right now, and one that survived a daemon restart would be a live
//!    approval nobody is watching.
//!
//! ## Why the request id is the capability
//!
//! `request_login` mints a 32-byte random id and a 6-digit code. The code is
//! for a *human* to compare across two screens, exactly as the BLE pairing
//! flow's Numeric Comparison does; it is short because a person reads it, and
//! it is therefore not a secret. The id is the secret: only the browser that
//! made the request knows it, so only that browser can collect the session the
//! approval mints. An attacker who starts their own request gets their own
//! code, which is what makes the comparison work — the parent's screen and the
//! attacker's screen show different numbers.

use chrono::{DateTime, Duration as ChronoDuration, Local};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use shepherd_util::{ProtectedFile, ProtectedFiles};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use thiserror::Error;
use tracing::{info, warn};

/// How long a pending companion-approval request stays approvable.
///
/// Short on purpose: the parent is holding the phone while the browser waits.
/// Anything longer is an approval sitting around for a login nobody is
/// performing.
pub const LOGIN_REQUEST_TTL: Duration = Duration::from_secs(120);

/// How often a browser is expected to poll a pending request. Used only to
/// size the sweep, which must not reap a request between two polls.
pub const LOGIN_POLL_INTERVAL: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

/// The knobs, resolved from `[service.management_api.auth]`.
#[derive(Debug, Clone)]
pub struct WebAuthPolicy {
    /// A session unused for this long is dead, however recently it was minted.
    pub session_idle: Duration,
    /// A session older than this is dead, however actively it is used.
    pub session_max_age: Duration,
    /// Consecutive failures from one peer before that peer is locked out.
    pub lockout_after: u32,
    /// How long the lockout lasts. Doubles per subsequent lockout, to a
    /// ceiling of 16x, and resets on a success.
    pub lockout: Duration,
}

impl Default for WebAuthPolicy {
    fn default() -> Self {
        Self {
            session_idle: Duration::from_secs(14 * 24 * 3600),
            session_max_age: Duration::from_secs(90 * 24 * 3600),
            lockout_after: 8,
            lockout: Duration::from_secs(300),
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum WebAuthError {
    #[error("web authentication is not set up on this device yet")]
    NotConfigured,
    #[error("web authentication is already set up; use the password to sign in")]
    AlreadyConfigured,
    #[error("incorrect password")]
    BadPassword,
    #[error("incorrect setup code")]
    BadEnrolmentCode,
    #[error("too many attempts; try again in {}s", .0.as_secs())]
    LockedOut(Duration),
    #[error("no such login request, or it expired")]
    NoSuchRequest,
    #[error("no such session")]
    NoSuchSession,
    #[error("password must be at least {0} characters")]
    PasswordTooShort(usize),
    #[error("web auth store error: {0}")]
    Store(String),
}

/// Shortest password the device will accept.
///
/// Eight, not twelve: the attacker who matters is on the LAN and rate-limited
/// to a handful of guesses per five minutes, not offline against a stolen
/// hash. A floor a parent will actually clear beats one they work around.
pub const MIN_PASSWORD_LEN: usize = 8;

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// What a client may know about the device's authentication state *before* it
/// has authenticated. Deliberately thin — it says which door to knock on and
/// nothing else.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WebAuthStatus {
    /// False on a device where nobody has set a password yet: the browser
    /// should show the setup screen and ask for the code on the TV.
    pub configured: bool,
    /// Whether a paired companion exists to approve a login. False means the
    /// password is the only way in, so the UI should not offer the other.
    pub companion_available: bool,
}

/// One live browser session, as an administrator sees it.
///
/// Carries no credential: `id` is a public handle used to revoke the session,
/// not the token that authenticates it. The token itself is stored hashed and
/// is never readable back out of this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WebSessionInfo {
    pub id: String,
    /// Human label derived from the User-Agent at login — "Chrome on Android",
    /// not a hex string, because the person revoking sessions is choosing
    /// between their own devices.
    pub label: String,
    /// The address the session logged in from, for the same reason.
    pub peer: String,
    pub created_at: DateTime<Local>,
    pub last_seen: DateTime<Local>,
    pub expires_at: DateTime<Local>,
    /// True for the session making the request, so the UI can label it and
    /// warn before revoking it.
    pub current: bool,
}

/// A login waiting on a tap in the companion app.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LoginRequestInfo {
    /// The request's public handle — what `approve_login_request` takes.
    ///
    /// Not the same string the browser polls with. The browser's id is a
    /// secret capability; this is a short opaque handle derived from it, so
    /// that listing pending requests over BLE does not hand out the ability to
    /// collect the resulting session.
    pub id: String,
    /// The six digits the browser is displaying. The parent compares.
    pub code: String,
    /// Who is asking, as best the device can tell: "Chrome on Android".
    pub label: String,
    /// The address the request came from.
    pub peer: String,
    pub requested_at: DateTime<Local>,
    pub expires_at: DateTime<Local>,
}

/// A freshly minted session: the only moment the token exists in the clear.
#[derive(Debug, Clone)]
pub struct MintedSession {
    /// The bearer/cookie value handed to exactly one client, once.
    pub token: String,
    pub info: WebSessionInfo,
}

/// The answer to "has my login been approved yet?".
#[derive(Debug)]
pub enum LoginPoll {
    Pending,
    Approved(Box<MintedSession>),
    Denied,
    Expired,
}

// ---------------------------------------------------------------------------
// Persisted shape
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoredFile {
    #[serde(default)]
    password: Option<StoredPassword>,
    /// Present only while `password` is not: the code shown on the device's
    /// own screen for first-run enrolment. Persisted so that a daemon restart
    /// does not invalidate the code a parent is in the middle of typing.
    #[serde(default)]
    enrolment_code: Option<String>,
    #[serde(default)]
    sessions: Vec<StoredSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredPassword {
    /// PHC string — algorithm, parameters and salt travel with the hash, so
    /// raising the cost later re-hashes on next login instead of locking
    /// everyone out.
    phc: String,
    updated_at: DateTime<Local>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredSession {
    id: String,
    /// SHA-256 of the token, hex. The file is protected, but a protected file
    /// full of live credentials is still a file full of live credentials — a
    /// backup, a support bundle or a bug that prints it should not hand
    /// anybody a session.
    token_sha256: String,
    label: String,
    peer: String,
    created_at: DateTime<Local>,
    last_seen: DateTime<Local>,
    expires_at: DateTime<Local>,
}

// ---------------------------------------------------------------------------
// In-memory state
// ---------------------------------------------------------------------------

struct PendingRequest {
    /// The browser's secret capability. Hashed, like a session token: this
    /// struct is not persisted, but the same reasoning applies to a core dump.
    poll_sha256: String,
    /// Public handle, safe to list.
    handle: String,
    code: String,
    label: String,
    peer: String,
    requested_at: DateTime<Local>,
    expires: Instant,
    state: RequestState,
}

enum RequestState {
    Pending,
    /// Approved, session minted, waiting for the browser's next poll to
    /// collect it. Held exactly once — the poll takes it.
    Approved(Box<MintedSession>),
    Denied,
}

#[derive(Default)]
struct Attempts {
    consecutive_failures: u32,
    lockouts: u32,
    locked_until: Option<Instant>,
}

struct Inner {
    stored: StoredFile,
    requests: Vec<PendingRequest>,
    attempts: HashMap<String, Attempts>,
    /// `last_seen` bumps that have not been written yet, and when the oldest
    /// of them happened. See [`WebAuth::touch_flush_due`].
    unflushed_since: Option<Instant>,
}

// ---------------------------------------------------------------------------
// WebAuth
// ---------------------------------------------------------------------------

/// The device's web-management credential store.
///
/// Synchronous throughout, behind a `std::sync::RwLock`, for the same reason
/// [`crate::AdminAuthority`] is: the HTTP auth middleware runs per request and
/// must not await. Every operation holds the lock for an in-memory update plus,
/// on the paths that change something durable, one write through
/// [`ProtectedFiles`].
pub struct WebAuth {
    files: Arc<dyn ProtectedFiles>,
    policy: WebAuthPolicy,
    inner: RwLock<Inner>,
    /// Whether a paired companion exists to approve logins. Read from the BLE
    /// claim machine, when there is one.
    companion: RwLock<Option<Arc<dyn crate::AdminAuthority>>>,
}

impl WebAuth {
    /// Load the store, generating a first-run enrolment code if no password
    /// has ever been set.
    pub fn load(
        files: Arc<dyn ProtectedFiles>,
        policy: WebAuthPolicy,
    ) -> Result<Self, WebAuthError> {
        let mut stored: StoredFile = match files
            .read(ProtectedFile::WebAuth)
            .map_err(|e| WebAuthError::Store(e.to_string()))?
        {
            Some(text) => toml::from_str(&text).map_err(|e| WebAuthError::Store(e.to_string()))?,
            None => StoredFile::default(),
        };

        let mut dirty = false;
        if stored.password.is_none() && stored.enrolment_code.is_none() {
            stored.enrolment_code = Some(numeric_code());
            dirty = true;
        }
        // A password that arrived by some other route (the companion) retires
        // the enrolment code rather than leaving a second door open.
        if stored.password.is_some() && stored.enrolment_code.is_some() {
            stored.enrolment_code = None;
            dirty = true;
        }

        let this = Self {
            files,
            policy,
            inner: RwLock::new(Inner {
                stored,
                requests: Vec::new(),
                attempts: HashMap::new(),
                unflushed_since: None,
            }),
            companion: RwLock::new(None),
        };
        if dirty {
            this.persist(&this.inner.read().expect("fresh lock").stored)?;
        }
        Ok(this)
    }

    /// Plug in the BLE claim machine, so [`WebAuthStatus::companion_available`]
    /// can tell a browser whether the "approve on my phone" button will lead
    /// anywhere.
    pub fn set_companion(&self, companion: Option<Arc<dyn crate::AdminAuthority>>) {
        *self.companion.write().expect("companion lock poisoned") = companion;
    }

    fn companion_available(&self) -> bool {
        self.companion
            .read()
            .expect("companion lock poisoned")
            .as_ref()
            .and_then(|c| c.current_http_token())
            .is_some()
    }

    pub fn status(&self) -> WebAuthStatus {
        let inner = self.inner.read().expect("web auth lock poisoned");
        WebAuthStatus {
            configured: inner.stored.password.is_some(),
            companion_available: self.companion_available(),
        }
    }

    /// The code to show on the device's own screen, or `None` once a password
    /// exists. The one moment the TV is a trusted channel is before the child
    /// has met the device, which is exactly when this is `Some`.
    pub fn enrolment_code(&self) -> Option<String> {
        self.inner
            .read()
            .expect("web auth lock poisoned")
            .stored
            .enrolment_code
            .clone()
    }

    // -- password ----------------------------------------------------------

    /// First-run enrolment: present the code from the TV, set the password,
    /// and get a session in the same breath so the browser is not asked to log
    /// in again immediately.
    pub fn complete_enrolment(
        &self,
        code: &str,
        password: &str,
        label: &str,
        peer: &str,
    ) -> Result<MintedSession, WebAuthError> {
        check_password_len(password)?;
        let mut inner = self.inner.write().expect("web auth lock poisoned");
        self.check_throttle(&mut inner, peer)?;

        let Some(expected) = inner.stored.enrolment_code.clone() else {
            return Err(WebAuthError::AlreadyConfigured);
        };
        if !constant_time_eq(code.trim(), &expected) {
            self.record_failure(&mut inner, peer);
            return Err(WebAuthError::BadEnrolmentCode);
        }

        inner.stored.password = Some(StoredPassword {
            phc: hash_password(password)?,
            updated_at: shepherd_util::now(),
        });
        inner.stored.enrolment_code = None;
        self.record_success(&mut inner, peer);
        let minted = mint_session(&mut inner.stored, &self.policy, label, peer);
        self.persist(&inner.stored)?;
        info!(%peer, "Web management password set from the first-run setup code");
        Ok(minted)
    }

    /// Set or replace the password without presenting the old one.
    ///
    /// The caller has already proved they are the administrator by some other
    /// means — a paired companion over an authenticated BLE link, or an
    /// operator with the reset sentinel. Every existing session survives: this
    /// is "I changed the password", not "I was compromised". Revoking is a
    /// separate, explicit act.
    pub fn set_password(&self, password: &str) -> Result<(), WebAuthError> {
        check_password_len(password)?;
        let mut inner = self.inner.write().expect("web auth lock poisoned");
        inner.stored.password = Some(StoredPassword {
            phc: hash_password(password)?,
            updated_at: shepherd_util::now(),
        });
        inner.stored.enrolment_code = None;
        inner.attempts.clear();
        self.persist(&inner.stored)?;
        info!("Web management password set by an authenticated administrator");
        Ok(())
    }

    /// Forget the password and every session, and mint a fresh enrolment code.
    ///
    /// The recovery path: `shepherdd`'s reset sentinel, or an operator over
    /// SSH. Sessions go with it — a password reset the owner did not perform
    /// is exactly the case where the live sessions are the problem.
    pub fn reset(&self) -> Result<String, WebAuthError> {
        let mut inner = self.inner.write().expect("web auth lock poisoned");
        inner.stored.password = None;
        inner.stored.sessions.clear();
        inner.requests.clear();
        inner.attempts.clear();
        let code = numeric_code();
        inner.stored.enrolment_code = Some(code.clone());
        self.persist(&inner.stored)?;
        warn!("Web management authentication reset; a new setup code is on the device screen");
        Ok(code)
    }

    /// Exchange a password for a session.
    pub fn login(
        &self,
        password: &str,
        label: &str,
        peer: &str,
    ) -> Result<MintedSession, WebAuthError> {
        let mut inner = self.inner.write().expect("web auth lock poisoned");
        self.check_throttle(&mut inner, peer)?;
        let Some(stored) = inner.stored.password.clone() else {
            return Err(WebAuthError::NotConfigured);
        };
        if !verify_password(&stored.phc, password) {
            self.record_failure(&mut inner, peer);
            warn!(%peer, "Rejected web management login: wrong password");
            return Err(WebAuthError::BadPassword);
        }
        self.record_success(&mut inner, peer);
        let minted = mint_session(&mut inner.stored, &self.policy, label, peer);
        self.persist(&inner.stored)?;
        info!(%peer, label, "Web management login");
        Ok(minted)
    }

    // -- sessions ----------------------------------------------------------

    /// Resolve a presented token to a live session, bumping its `last_seen`.
    ///
    /// Returns the session's public id, which callers keep so they can mark it
    /// `current` in a listing and revoke it on sign-out. `None` covers every
    /// kind of no: unknown, expired, idle out, revoked.
    ///
    /// **Does not write.** `last_seen` is updated in memory and flushed by
    /// [`Self::flush_due`] on the daemon's sweep, because writing through the
    /// custodian on every authenticated request would turn the management API
    /// into a write amplifier for a field nobody reads in real time.
    pub fn resolve(&self, presented: &str) -> Option<String> {
        let digest = sha256_hex(presented);
        let now = shepherd_util::now();
        let mut inner = self.inner.write().expect("web auth lock poisoned");
        let idle = chrono_duration(self.policy.session_idle);
        let session = inner
            .stored
            .sessions
            .iter_mut()
            .find(|s| constant_time_eq(&s.token_sha256, &digest))?;
        if now >= session.expires_at || now - session.last_seen > idle {
            let id = session.id.clone();
            inner.stored.sessions.retain(|s| s.id != id);
            let stored = inner.stored.clone();
            drop(inner);
            let _ = self.persist(&stored);
            return None;
        }
        session.last_seen = now;
        let id = session.id.clone();
        inner.unflushed_since.get_or_insert_with(Instant::now);
        Some(id)
    }

    /// End one session by its token — the browser signing itself out.
    pub fn sign_out(&self, presented: &str) -> Result<(), WebAuthError> {
        let digest = sha256_hex(presented);
        let mut inner = self.inner.write().expect("web auth lock poisoned");
        let before = inner.stored.sessions.len();
        inner
            .stored
            .sessions
            .retain(|s| !bool::from(s.token_sha256.as_bytes().ct_eq(digest.as_bytes())));
        if inner.stored.sessions.len() == before {
            return Err(WebAuthError::NoSuchSession);
        }
        self.persist(&inner.stored)
    }

    pub fn list_sessions(&self, current: Option<&str>) -> Vec<WebSessionInfo> {
        let inner = self.inner.read().expect("web auth lock poisoned");
        inner
            .stored
            .sessions
            .iter()
            .map(|s| session_info(s, current))
            .collect()
    }

    /// Revoke one session by its public id — the administrator ending a
    /// session on a device they no longer have.
    pub fn revoke_session(&self, id: &str) -> Result<(), WebAuthError> {
        let mut inner = self.inner.write().expect("web auth lock poisoned");
        let before = inner.stored.sessions.len();
        inner.stored.sessions.retain(|s| s.id != id);
        if inner.stored.sessions.len() == before {
            return Err(WebAuthError::NoSuchSession);
        }
        self.persist(&inner.stored)?;
        info!(session = %id, "Web management session revoked");
        Ok(())
    }

    /// Revoke every session, including the caller's.
    pub fn revoke_all_sessions(&self) -> Result<usize, WebAuthError> {
        let mut inner = self.inner.write().expect("web auth lock poisoned");
        let n = inner.stored.sessions.len();
        inner.stored.sessions.clear();
        self.persist(&inner.stored)?;
        warn!(count = n, "Every web management session revoked");
        Ok(n)
    }

    // -- companion approval ------------------------------------------------

    /// Start a login that a paired companion will approve.
    ///
    /// Returns the browser's polling capability and the six digits it should
    /// display. Rate-limited on the same counter as password attempts: a
    /// request is cheap to make and each one puts a line in front of the
    /// parent, so an unlimited stream of them is a denial of service against
    /// the approval screen.
    pub fn request_login(
        &self,
        label: &str,
        peer: &str,
    ) -> Result<(String, LoginRequestInfo), WebAuthError> {
        let mut inner = self.inner.write().expect("web auth lock poisoned");
        self.check_throttle(&mut inner, peer)?;
        self.sweep_requests(&mut inner);
        // Each request counts against the peer's budget whether or not it is
        // ever approved; an approval resets the counter.
        self.record_failure(&mut inner, peer);

        let poll_token = random_token();
        let handle = short_handle();
        let code = numeric_code();
        let requested_at = shepherd_util::now();
        let info = LoginRequestInfo {
            id: handle.clone(),
            code: code.clone(),
            label: label.to_string(),
            peer: peer.to_string(),
            requested_at,
            expires_at: requested_at + chrono_duration(LOGIN_REQUEST_TTL),
        };
        inner.requests.push(PendingRequest {
            poll_sha256: sha256_hex(&poll_token),
            handle,
            code,
            label: label.to_string(),
            peer: peer.to_string(),
            requested_at,
            expires: Instant::now() + LOGIN_REQUEST_TTL,
            state: RequestState::Pending,
        });
        info!(%peer, label, "Web management login requested; awaiting companion approval");
        Ok((poll_token, info))
    }

    /// Collect the outcome of a request. Consumes an approval: the session is
    /// handed over exactly once, to whoever holds the polling capability.
    pub fn poll_login(&self, poll_token: &str) -> LoginPoll {
        let digest = sha256_hex(poll_token);
        let mut inner = self.inner.write().expect("web auth lock poisoned");
        self.sweep_requests(&mut inner);
        let Some(idx) = inner
            .requests
            .iter()
            .position(|r| constant_time_eq(&r.poll_sha256, &digest))
        else {
            return LoginPoll::Expired;
        };
        match &inner.requests[idx].state {
            RequestState::Pending => LoginPoll::Pending,
            RequestState::Denied => {
                inner.requests.remove(idx);
                LoginPoll::Denied
            }
            RequestState::Approved(_) => {
                let request = inner.requests.remove(idx);
                let RequestState::Approved(minted) = request.state else {
                    unreachable!("just matched Approved")
                };
                LoginPoll::Approved(minted)
            }
        }
    }

    pub fn list_login_requests(&self) -> Vec<LoginRequestInfo> {
        let mut inner = self.inner.write().expect("web auth lock poisoned");
        self.sweep_requests(&mut inner);
        inner
            .requests
            .iter()
            .filter(|r| matches!(r.state, RequestState::Pending))
            .map(|r| LoginRequestInfo {
                id: r.handle.clone(),
                code: r.code.clone(),
                label: r.label.clone(),
                peer: r.peer.clone(),
                requested_at: r.requested_at,
                expires_at: r.requested_at + chrono_duration(LOGIN_REQUEST_TTL),
            })
            .collect()
    }

    /// Approve a pending request, minting the session the browser will
    /// collect on its next poll. Called by an already-authenticated
    /// administrator — the companion over BLE, or a signed-in browser.
    pub fn approve_login_request(&self, handle: &str) -> Result<(), WebAuthError> {
        let mut inner = self.inner.write().expect("web auth lock poisoned");
        self.sweep_requests(&mut inner);
        let Some(idx) = inner.requests.iter().position(|r| r.handle == handle) else {
            return Err(WebAuthError::NoSuchRequest);
        };
        let (label, peer) = (
            inner.requests[idx].label.clone(),
            inner.requests[idx].peer.clone(),
        );
        let minted = mint_session(&mut inner.stored, &self.policy, &label, &peer);
        inner.requests[idx].state = RequestState::Approved(Box::new(minted));
        self.record_success(&mut inner, &peer);
        self.persist(&inner.stored)?;
        info!(%peer, label, "Web management login approved from the companion");
        Ok(())
    }

    pub fn deny_login_request(&self, handle: &str) -> Result<(), WebAuthError> {
        let mut inner = self.inner.write().expect("web auth lock poisoned");
        let Some(idx) = inner.requests.iter().position(|r| r.handle == handle) else {
            return Err(WebAuthError::NoSuchRequest);
        };
        inner.requests[idx].state = RequestState::Denied;
        warn!(handle, "Web management login denied from the companion");
        Ok(())
    }

    // -- housekeeping ------------------------------------------------------

    /// Drop expired sessions and requests, and flush pending `last_seen`
    /// bumps. Called on the daemon's minute tick; safe to call at any rate.
    pub fn sweep(&self) {
        let now = shepherd_util::now();
        let idle = chrono_duration(self.policy.session_idle);
        let mut inner = self.inner.write().expect("web auth lock poisoned");
        self.sweep_requests(&mut inner);
        let before = inner.stored.sessions.len();
        inner
            .stored
            .sessions
            .retain(|s| now < s.expires_at && now - s.last_seen <= idle);
        let expired = before - inner.stored.sessions.len();
        let flush_due = inner
            .unflushed_since
            .is_some_and(|t| t.elapsed() >= Duration::from_secs(60));
        if expired > 0 || flush_due {
            inner.unflushed_since = None;
            let stored = inner.stored.clone();
            drop(inner);
            if let Err(e) = self.persist(&stored) {
                warn!(error = %e, "Could not write the web auth store during sweep");
            }
            if expired > 0 {
                info!(count = expired, "Expired web management sessions");
            }
        }
    }

    fn sweep_requests(&self, inner: &mut Inner) {
        let now = Instant::now();
        inner.requests.retain(|r| r.expires > now);
    }

    fn persist(&self, stored: &StoredFile) -> Result<(), WebAuthError> {
        let text =
            toml::to_string_pretty(stored).map_err(|e| WebAuthError::Store(e.to_string()))?;
        self.files
            .write(ProtectedFile::WebAuth, &text)
            .map_err(|e| WebAuthError::Store(e.to_string()))
    }

    // -- throttle ----------------------------------------------------------

    fn check_throttle(&self, inner: &mut Inner, peer: &str) -> Result<(), WebAuthError> {
        if let Some(a) = inner.attempts.get_mut(peer)
            && let Some(until) = a.locked_until
        {
            let now = Instant::now();
            if until > now {
                return Err(WebAuthError::LockedOut(until - now));
            }
            a.locked_until = None;
            a.consecutive_failures = 0;
        }
        Ok(())
    }

    fn record_failure(&self, inner: &mut Inner, peer: &str) {
        let after = self.policy.lockout_after;
        let base = self.policy.lockout;
        let a = inner.attempts.entry(peer.to_string()).or_default();
        a.consecutive_failures += 1;
        if a.consecutive_failures >= after {
            // Backoff doubles per lockout to a 16x ceiling, so a patient
            // guesser is spending hours per handful of attempts while a parent
            // who mistyped twice is waiting minutes.
            let factor = 1u32 << a.lockouts.min(4);
            a.locked_until = Some(Instant::now() + base * factor);
            a.lockouts += 1;
            a.consecutive_failures = 0;
            warn!(%peer, seconds = (base * factor).as_secs(), "Locked out of web management login");
        }
    }

    fn record_success(&self, inner: &mut Inner, peer: &str) {
        inner.attempts.remove(peer);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn session_info(s: &StoredSession, current: Option<&str>) -> WebSessionInfo {
    WebSessionInfo {
        id: s.id.clone(),
        label: s.label.clone(),
        peer: s.peer.clone(),
        created_at: s.created_at,
        last_seen: s.last_seen,
        expires_at: s.expires_at,
        current: current == Some(s.id.as_str()),
    }
}

fn mint_session(
    stored: &mut StoredFile,
    policy: &WebAuthPolicy,
    label: &str,
    peer: &str,
) -> MintedSession {
    let token = random_token();
    let now = shepherd_util::now();
    let record = StoredSession {
        id: short_handle(),
        token_sha256: sha256_hex(&token),
        label: label.to_string(),
        peer: peer.to_string(),
        created_at: now,
        last_seen: now,
        expires_at: now + chrono_duration(policy.session_max_age),
    };
    let info = session_info(&record, Some(&record.id));
    stored.sessions.push(record);
    MintedSession { token, info }
}

fn check_password_len(password: &str) -> Result<(), WebAuthError> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(WebAuthError::PasswordTooShort(MIN_PASSWORD_LEN));
    }
    Ok(())
}

fn hash_password(password: &str) -> Result<String, WebAuthError> {
    use argon2::password_hash::{PasswordHasher, SaltString, rand_core::OsRng};
    let salt = SaltString::generate(&mut OsRng);
    argon2::Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| WebAuthError::Store(e.to_string()))
}

fn verify_password(phc: &str, password: &str) -> bool {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    let Ok(parsed) = PasswordHash::new(phc) else {
        return false;
    };
    argon2::Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// 32 bytes of CSPRNG, URL-safe base64 — a session token or a polling
/// capability. Unguessable is the whole specification.
fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64_url(&bytes)
}

/// A short public handle: long enough not to collide, not a credential.
fn short_handle() -> String {
    let mut bytes = [0u8; 9];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64_url(&bytes)
}

/// Six digits, uniformly, for a human to compare across two screens.
fn numeric_code() -> String {
    // Rejection sampling rather than `% 1_000_000`, which would make the low
    // codes fractionally likelier. It costs nothing here and the alternative
    // is the kind of bias that is embarrassing to explain later.
    loop {
        let mut bytes = [0u8; 4];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        let n = u32::from_le_bytes(bytes);
        if n < 4_294_000_000 {
            return format!("{:06}", n % 1_000_000);
        }
    }
}

fn base64_url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let chars = [
            ALPHABET[(n >> 18) as usize & 63],
            ALPHABET[(n >> 12) as usize & 63],
            ALPHABET[(n >> 6) as usize & 63],
            ALPHABET[n as usize & 63],
        ];
        let keep = match chunk.len() {
            1 => 2,
            2 => 3,
            _ => 4,
        };
        for c in &chars[..keep] {
            out.push(*c as char);
        }
    }
    out
}

fn sha256_hex(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    // Lengths are public here (a hex digest, a six-digit code), so the early
    // length check leaks nothing the caller did not already know.
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

fn chrono_duration(d: Duration) -> ChronoDuration {
    ChronoDuration::from_std(d).unwrap_or_else(|_| ChronoDuration::days(365))
}

/// Turn a User-Agent into something a person can pick their own device out of.
///
/// Deliberately crude. This is a label in a list next to a revoke button, not
/// telemetry, and the failure mode of guessing wrong is a row that reads
/// "Unknown browser" instead of "Safari".
pub fn label_from_user_agent(ua: Option<&str>) -> String {
    let Some(ua) = ua.filter(|s| !s.is_empty()) else {
        return "Unknown browser".to_string();
    };
    let browser = [
        ("Edg/", "Edge"),
        ("OPR/", "Opera"),
        ("Firefox/", "Firefox"),
        ("Chrome/", "Chrome"),
        ("Safari/", "Safari"),
    ]
    .iter()
    .find(|(needle, _)| ua.contains(needle))
    .map(|(_, name)| *name)
    .unwrap_or("Browser");
    let platform = [
        ("Android", "Android"),
        ("iPhone", "iPhone"),
        ("iPad", "iPad"),
        ("Macintosh", "macOS"),
        ("Windows", "Windows"),
        ("Linux", "Linux"),
    ]
    .iter()
    .find(|(needle, _)| ua.contains(needle))
    .map(|(_, name)| *name);
    match platform {
        Some(p) => format!("{browser} on {p}"),
        None => browser.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shepherd_util::LocalProtectedFiles;
    use tempfile::TempDir;

    fn store(dir: &TempDir) -> Arc<dyn ProtectedFiles> {
        Arc::new(LocalProtectedFiles::new(dir.path().to_path_buf()))
    }

    fn auth(dir: &TempDir) -> WebAuth {
        WebAuth::load(store(dir), WebAuthPolicy::default()).expect("loads")
    }

    fn enrolled(dir: &TempDir) -> WebAuth {
        let a = auth(dir);
        let code = a.enrolment_code().expect("fresh store has a code");
        a.complete_enrolment(
            &code,
            "correct horse battery",
            "Firefox on Linux",
            "1.2.3.4",
        )
        .expect("enrolment succeeds");
        a
    }

    #[test]
    fn a_fresh_store_is_unconfigured_and_has_a_setup_code() {
        let dir = TempDir::new().unwrap();
        let a = auth(&dir);
        assert!(!a.status().configured);
        let code = a.enrolment_code().expect("a code");
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn the_setup_code_survives_a_restart() {
        let dir = TempDir::new().unwrap();
        let first = auth(&dir).enrolment_code().unwrap();
        // A parent halfway through typing the code should not be defeated by a
        // daemon restart, so the code is persisted rather than per-process.
        assert_eq!(auth(&dir).enrolment_code().unwrap(), first);
    }

    #[test]
    fn enrolment_sets_the_password_and_retires_the_code() {
        let dir = TempDir::new().unwrap();
        let a = enrolled(&dir);
        assert!(a.status().configured);
        assert!(a.enrolment_code().is_none());
        // And it stays retired across a reload.
        assert!(auth(&dir).enrolment_code().is_none());
    }

    #[test]
    fn a_wrong_setup_code_is_refused() {
        let dir = TempDir::new().unwrap();
        let a = auth(&dir);
        let err = a
            .complete_enrolment("000000", "correct horse battery", "l", "p")
            .unwrap_err();
        assert!(matches!(err, WebAuthError::BadEnrolmentCode));
        assert!(!a.status().configured);
    }

    #[test]
    fn a_short_password_is_refused_before_the_code_is_even_checked() {
        let dir = TempDir::new().unwrap();
        let a = auth(&dir);
        let code = a.enrolment_code().unwrap();
        assert!(matches!(
            a.complete_enrolment(&code, "short", "l", "p").unwrap_err(),
            WebAuthError::PasswordTooShort(_)
        ));
        // The code survives a rejected attempt — otherwise a typo in the
        // password would burn the one credential the parent has.
        assert_eq!(a.enrolment_code().unwrap(), code);
    }

    #[test]
    fn login_mints_a_session_that_resolves() {
        let dir = TempDir::new().unwrap();
        let a = enrolled(&dir);
        let minted = a
            .login("correct horse battery", "Chrome on Android", "1.2.3.4")
            .expect("login");
        assert_eq!(
            a.resolve(&minted.token).as_deref(),
            Some(minted.info.id.as_str())
        );
    }

    #[test]
    fn a_wrong_password_mints_nothing() {
        let dir = TempDir::new().unwrap();
        let a = enrolled(&dir);
        // Enrolment itself signed the setting-up browser in, so the count to
        // hold still is one, not zero.
        let before = a.list_sessions(None).len();
        assert!(matches!(
            a.login("wrong password here", "l", "p").unwrap_err(),
            WebAuthError::BadPassword
        ));
        assert_eq!(a.list_sessions(None).len(), before);
    }

    #[test]
    fn the_token_is_never_stored_in_the_clear() {
        let dir = TempDir::new().unwrap();
        let a = enrolled(&dir);
        let minted = a.login("correct horse battery", "l", "p").unwrap();
        let on_disk = std::fs::read_to_string(dir.path().join("web-auth.toml")).unwrap();
        assert!(
            !on_disk.contains(&minted.token),
            "the session token is on disk in the clear"
        );
        assert!(!on_disk.contains("correct horse battery"));
    }

    #[test]
    fn sessions_survive_a_restart() {
        let dir = TempDir::new().unwrap();
        let minted = {
            let a = enrolled(&dir);
            a.login("correct horse battery", "l", "p").unwrap()
        };
        assert!(auth(&dir).resolve(&minted.token).is_some());
    }

    #[test]
    fn signing_out_ends_exactly_one_session() {
        let dir = TempDir::new().unwrap();
        let a = enrolled(&dir);
        let one = a.login("correct horse battery", "one", "p").unwrap();
        let two = a.login("correct horse battery", "two", "p").unwrap();
        a.sign_out(&one.token).expect("sign out");
        assert!(a.resolve(&one.token).is_none());
        assert!(a.resolve(&two.token).is_some());
    }

    #[test]
    fn revoking_by_id_ends_a_session_the_admin_cannot_reach() {
        let dir = TempDir::new().unwrap();
        let a = enrolled(&dir);
        let lost = a
            .login("correct horse battery", "stolen laptop", "p")
            .unwrap();
        a.revoke_session(&lost.info.id).expect("revoke");
        assert!(a.resolve(&lost.token).is_none());
        assert!(matches!(
            a.revoke_session(&lost.info.id).unwrap_err(),
            WebAuthError::NoSuchSession
        ));
    }

    #[test]
    fn an_expired_session_stops_resolving() {
        let dir = TempDir::new().unwrap();
        let a = WebAuth::load(
            store(&dir),
            WebAuthPolicy {
                session_max_age: Duration::from_secs(0),
                ..WebAuthPolicy::default()
            },
        )
        .unwrap();
        let code = a.enrolment_code().unwrap();
        let minted = a
            .complete_enrolment(&code, "correct horse battery", "l", "p")
            .unwrap();
        assert!(a.resolve(&minted.token).is_none());
    }

    #[test]
    fn an_idle_session_stops_resolving() {
        let dir = TempDir::new().unwrap();
        let a = WebAuth::load(
            store(&dir),
            WebAuthPolicy {
                // Idle timeout shorter than the absolute one, so this test can
                // only pass through the idle branch.
                session_idle: Duration::from_secs(0),
                ..WebAuthPolicy::default()
            },
        )
        .unwrap();
        let code = a.enrolment_code().unwrap();
        let minted = a
            .complete_enrolment(&code, "correct horse battery", "l", "p")
            .unwrap();
        // `last_seen` is set at mint time, so a zero idle window is already
        // exceeded by the time anything asks.
        std::thread::sleep(Duration::from_millis(1100));
        assert!(a.resolve(&minted.token).is_none());
    }

    #[test]
    fn repeated_failures_lock_the_peer_out_and_the_lock_names_its_own_duration() {
        let dir = TempDir::new().unwrap();
        let a = WebAuth::load(
            store(&dir),
            WebAuthPolicy {
                lockout_after: 3,
                lockout: Duration::from_secs(60),
                ..WebAuthPolicy::default()
            },
        )
        .unwrap();
        let code = a.enrolment_code().unwrap();
        a.complete_enrolment(&code, "correct horse battery", "l", "setup")
            .unwrap();

        for _ in 0..3 {
            assert!(matches!(
                a.login("wrong password here", "l", "guesser").unwrap_err(),
                WebAuthError::BadPassword
            ));
        }
        let err = a
            .login("correct horse battery", "l", "guesser")
            .unwrap_err();
        let WebAuthError::LockedOut(remaining) = err else {
            panic!("expected a lockout, got {err:?}");
        };
        assert!(remaining <= Duration::from_secs(60) && remaining > Duration::from_secs(50));

        // The lockout is per peer: another address is unaffected, which is what
        // keeps a guesser from locking the parent out of their own device.
        assert!(a.login("correct horse battery", "l", "the parent").is_ok());
    }

    #[test]
    fn a_successful_login_clears_the_failure_count() {
        let dir = TempDir::new().unwrap();
        let a = WebAuth::load(
            store(&dir),
            WebAuthPolicy {
                lockout_after: 3,
                ..WebAuthPolicy::default()
            },
        )
        .unwrap();
        let code = a.enrolment_code().unwrap();
        a.complete_enrolment(&code, "correct horse battery", "l", "setup")
            .unwrap();
        for _ in 0..2 {
            let _ = a.login("wrong password here", "l", "typo");
        }
        assert!(a.login("correct horse battery", "l", "typo").is_ok());
        // Two more failures would lock out if the counter had not been reset.
        for _ in 0..2 {
            let _ = a.login("wrong password here", "l", "typo");
        }
        assert!(a.login("correct horse battery", "l", "typo").is_ok());
    }

    #[test]
    fn a_companion_approval_hands_the_session_to_the_polling_browser() {
        let dir = TempDir::new().unwrap();
        let a = enrolled(&dir);
        let (poll_token, info) = a.request_login("Chrome on Android", "1.2.3.4").unwrap();
        assert!(matches!(a.poll_login(&poll_token), LoginPoll::Pending));

        let listed = a.list_login_requests();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].code, info.code);
        // What the companion lists must not be what the browser polls with,
        // or listing pending requests would hand out the sessions.
        assert_ne!(listed[0].id, poll_token);

        a.approve_login_request(&listed[0].id).expect("approve");
        let LoginPoll::Approved(minted) = a.poll_login(&poll_token) else {
            panic!("expected an approved poll");
        };
        assert!(a.resolve(&minted.token).is_some());
        // The approval is collected exactly once.
        assert!(matches!(a.poll_login(&poll_token), LoginPoll::Expired));
    }

    #[test]
    fn a_denied_request_tells_the_browser_so() {
        let dir = TempDir::new().unwrap();
        let a = enrolled(&dir);
        let (poll_token, _) = a.request_login("l", "p").unwrap();
        let handle = a.list_login_requests()[0].id.clone();
        a.deny_login_request(&handle).expect("deny");
        assert!(matches!(a.poll_login(&poll_token), LoginPoll::Denied));
    }

    #[test]
    fn two_concurrent_requests_get_different_codes_and_their_own_sessions() {
        // The property the whole numeric-comparison design rests on: an
        // attacker racing the parent shows a different number, so the parent
        // approving "their" code cannot approve the attacker's request.
        let dir = TempDir::new().unwrap();
        let a = enrolled(&dir);
        let (parent_poll, parent_info) = a.request_login("parent", "1.1.1.1").unwrap();
        let (attacker_poll, attacker_info) = a.request_login("attacker", "2.2.2.2").unwrap();
        assert_ne!(parent_info.code, attacker_info.code);

        let listed = a.list_login_requests();
        let parent_handle = listed
            .iter()
            .find(|r| r.code == parent_info.code)
            .expect("the parent's request")
            .id
            .clone();
        a.approve_login_request(&parent_handle).unwrap();

        assert!(matches!(a.poll_login(&parent_poll), LoginPoll::Approved(_)));
        assert!(matches!(a.poll_login(&attacker_poll), LoginPoll::Pending));
    }

    #[test]
    fn an_unknown_poll_token_is_never_pending() {
        let dir = TempDir::new().unwrap();
        let a = enrolled(&dir);
        assert!(matches!(a.poll_login("nonsense"), LoginPoll::Expired));
    }

    #[test]
    fn setting_a_password_from_the_companion_keeps_existing_sessions() {
        let dir = TempDir::new().unwrap();
        let a = enrolled(&dir);
        let minted = a.login("correct horse battery", "l", "p").unwrap();
        a.set_password("a whole new password").expect("set");
        assert!(a.resolve(&minted.token).is_some());
        assert!(a.login("a whole new password", "l", "p").is_ok());
        assert!(a.login("correct horse battery", "l", "p").is_err());
    }

    #[test]
    fn a_reset_drops_every_session_and_mints_a_new_setup_code() {
        let dir = TempDir::new().unwrap();
        let a = enrolled(&dir);
        let minted = a.login("correct horse battery", "l", "p").unwrap();
        let code = a.reset().expect("reset");
        assert!(a.resolve(&minted.token).is_none());
        assert!(!a.status().configured);
        assert_eq!(a.enrolment_code().as_deref(), Some(code.as_str()));
    }

    #[test]
    fn revoke_all_clears_the_lot() {
        let dir = TempDir::new().unwrap();
        let a = enrolled(&dir);
        let one = a.login("correct horse battery", "one", "p").unwrap();
        let two = a.login("correct horse battery", "two", "p").unwrap();
        // Three, not two: enrolment signed in the browser that set the
        // password, and "revoke everything" means everything.
        assert_eq!(a.revoke_all_sessions().unwrap(), 3);
        assert!(a.resolve(&one.token).is_none());
        assert!(a.resolve(&two.token).is_none());
    }

    #[test]
    fn the_current_session_is_marked_in_a_listing() {
        let dir = TempDir::new().unwrap();
        let a = enrolled(&dir);
        let one = a.login("correct horse battery", "one", "p").unwrap();
        let _two = a.login("correct horse battery", "two", "p").unwrap();
        let listed = a.list_sessions(Some(&one.info.id));
        assert_eq!(listed.iter().filter(|s| s.current).count(), 1);
        assert!(listed.iter().find(|s| s.current).unwrap().id == one.info.id);
    }

    #[test]
    fn user_agent_labels_are_recognisable() {
        assert_eq!(
            label_from_user_agent(Some(
                "Mozilla/5.0 (Linux; Android 14) AppleWebKit/537.36 Chrome/120.0 Mobile Safari/537.36"
            )),
            "Chrome on Android"
        );
        assert_eq!(
            label_from_user_agent(Some("Mozilla/5.0 (X11; Linux x86_64) Firefox/121.0")),
            "Firefox on Linux"
        );
        assert_eq!(label_from_user_agent(None), "Unknown browser");
        assert_eq!(label_from_user_agent(Some("")), "Unknown browser");
    }

    #[test]
    fn codes_are_six_digits_and_not_all_the_same() {
        let codes: std::collections::HashSet<String> = (0..64).map(|_| numeric_code()).collect();
        assert!(codes.len() > 32, "codes look far from uniform");
        assert!(codes.iter().all(|c| c.len() == 6));
    }

    #[test]
    fn base64_url_round_trips_lengths() {
        // Not a full codec test: the property that matters is that a token is
        // long, URL-safe, and never padded into something a cookie mangles.
        for len in [1usize, 2, 3, 9, 32] {
            let s = base64_url(&vec![0xABu8; len]);
            assert_eq!(s.len(), len.div_ceil(3) * 4 - (3 - len % 3) % 3);
            assert!(
                s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            );
        }
    }
}

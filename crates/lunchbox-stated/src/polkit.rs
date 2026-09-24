//! Asking polkit, at startup, whether the watchdog can actually fire
//! (issue #172).
//!
//! Ending a session this daemon's uid does not own needs polkit's
//! `org.freedesktop.login1.manage`, granted by
//! `dist/polkit/50-lunchbox-session-guard.rules`. Without the rule everything
//! else still works — the connection is watched, the deadline runs, the log
//! line is written — and then `TerminateSession` comes back with an
//! authorisation error and the session carries on unsupervised.
//!
//! That is the worst available shape: a protection that reports itself as
//! present and is not. So it is checked once, at startup, and the answer is
//! sent back to lunchboxd — which is alive at that moment, and has a
//! diagnostics channel this daemon will not have when the watchdog is needed.
//!
//! The check is `AllowUserInteraction = 0`. There is nobody to prompt: this
//! daemon has no session, no seat and no agent, and a check that could prompt
//! would hang rather than answer.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use zbus::zvariant::{Type, Value};

/// The action that gates ending someone else's session.
const ACTION: &str = "org.freedesktop.login1.manage";

/// What polkit said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authority {
    /// The watchdog can end the session.
    Granted,
    /// It cannot, and the device should be told so while something is still
    /// able to say it.
    Denied,
    /// polkit could not be asked. **Treated as armed**: not knowing is not the
    /// same as knowing it will fail, and a watchdog that stood down because its
    /// self-check did not answer would be worse than one that tries and logs.
    Unknown(String),
}

impl Authority {
    /// Whether losing supervision is expected to actually end the session.
    pub fn armed(&self) -> bool {
        !matches!(self, Self::Denied)
    }

    /// What to tell lunchboxd, when there is something to tell it.
    pub fn caveat(&self) -> Option<String> {
        match self {
            Self::Granted => None,
            Self::Denied => Some(format!(
                "polkit refuses {ACTION} to this daemon's uid, so the session watchdog cannot \
                 end the session when lunchboxd stops supervising it. Install \
                 /etc/polkit-1/rules.d/50-lunchbox-session-guard.rules (issue #172)"
            )),
            Self::Unknown(why) => Some(format!(
                "could not ask polkit whether the session watchdog may end this session ({why}); \
                 it will try anyway when it has to"
            )),
        }
    }
}

/// polkit's authority, enough of it to ask one question.
#[zbus::proxy(
    interface = "org.freedesktop.PolicyKit1.Authority",
    default_service = "org.freedesktop.PolicyKit1",
    default_path = "/org/freedesktop/PolicyKit1/Authority"
)]
trait PolkitAuthority {
    fn check_authorization(
        &self,
        subject: &Subject<'_>,
        action_id: &str,
        details: HashMap<&str, &str>,
        flags: u32,
        cancellation_id: &str,
    ) -> zbus::Result<AuthorizationResult>;
}

/// Who is asking. `system-bus-name` rather than `unix-process`: polkit resolves
/// the name to a process through the bus daemon, so there is no pid and start
/// time to read out of `/proc` and no window in which either could be reused.
#[derive(Debug, Serialize, Type)]
struct Subject<'a> {
    kind: &'a str,
    details: HashMap<&'a str, Value<'a>>,
}

#[derive(Debug, Deserialize, Type)]
struct AuthorizationResult {
    is_authorized: bool,
    /// polkit would prompt. With no interaction allowed and no agent to prompt
    /// with, this is a refusal wearing a different word.
    is_challenge: bool,
    #[allow(dead_code)]
    details: HashMap<String, String>,
}

/// Ask polkit whether this daemon may end a session it does not own.
pub async fn may_end_sessions(conn: &zbus::Connection) -> Authority {
    match check(conn).await {
        Ok(true) => Authority::Granted,
        Ok(false) => Authority::Denied,
        Err(e) => Authority::Unknown(e.to_string()),
    }
}

async fn check(conn: &zbus::Connection) -> Result<bool> {
    check_action(conn, ACTION).await
}

/// Ask polkit whether this daemon holds `action`, without interaction.
///
/// Shared with the wireless grant (issue #194), which asks the same question
/// about a different action. `is_challenge` counts as a refusal: it means
/// polkit would prompt, and with no interaction allowed and no agent to prompt
/// with, that is a refusal wearing a different word.
pub async fn check_action(conn: &zbus::Connection, action: &str) -> Result<bool> {
    let unique = conn
        .unique_name()
        .context("this connection has no unique name to identify it by")?
        .to_string();
    let authority = PolkitAuthorityProxy::new(conn)
        .await
        .context("connecting to polkit")?;
    let subject = Subject {
        kind: "system-bus-name",
        details: HashMap::from([("name", Value::from(unique))]),
    };
    let result = authority
        .check_authorization(&subject, action, HashMap::new(), 0, "")
        .await
        .with_context(|| format!("asking polkit about {action}"))?;
    Ok(result.is_authorized && !result.is_challenge)
}

//! The server's `LaunchOutcome` and the IPC client's mirror of it must agree
//! on the wire.
//!
//! They did not, for as long as both existed: the server type is externally
//! tagged (`{"Approved": {…}}`) while `lunchbox_ipc::LaunchOutcome` was
//! `#[serde(untagged)]`, a shape that can never match. Every launch on
//! `copernicus` logged
//!
//! ```text
//! ERROR lunchbox_launcher::app: Launch failed on server
//!       error=JSON error: data did not match any variant of untagged enum LaunchOutcome
//! ```
//!
//! An approval survived it by accident — the launcher falls into its error arm,
//! re-fetches state, sees the session and carries on — but a *denial* did not.
//! `LaunchOutcome::Denied { reasons }` never decoded, so the launcher dropped
//! back to the grid with no explanation and a child who was out of time saw
//! their press do nothing at all.
//!
//! The two types live in crates that don't otherwise meet, which is how the
//! mismatch went unnoticed. This test is the seam.

use lunchbox_api::ReasonCode;
use lunchbox_management::LaunchOutcome as ServerOutcome;

/// Serialize exactly as lunchboxd does when answering `launch`.
fn on_the_wire(outcome: &ServerOutcome) -> String {
    serde_json::to_string(outcome).expect("server outcome serializes")
}

#[test]
fn client_decodes_an_approval_from_the_server() {
    let wire = on_the_wire(&ServerOutcome::Approved {
        session_id: "3288e400-38f6-415e-ad22-f89635794178".into(),
        deadline: None,
    });

    match serde_json::from_str::<lunchbox_ipc::LaunchOutcome>(&wire) {
        Ok(lunchbox_ipc::LaunchOutcome::Approved { session_id, .. }) => {
            assert_eq!(session_id, "3288e400-38f6-415e-ad22-f89635794178");
        }
        other => panic!("client could not read the server's approval: {other:?}\nwire: {wire}"),
    }
}

#[test]
fn client_decodes_a_denial_from_the_server() {
    let wire = on_the_wire(&ServerOutcome::Denied {
        reasons: vec![
            ReasonCode::OutsideTimeWindow {
                next_window_start: None,
            },
            ReasonCode::QuotaExhausted {
                used: std::time::Duration::from_secs(3600),
                quota: std::time::Duration::from_secs(1800),
            },
        ],
    });

    match serde_json::from_str::<lunchbox_ipc::LaunchOutcome>(&wire) {
        Ok(lunchbox_ipc::LaunchOutcome::Denied { reasons }) => {
            assert_eq!(
                reasons.len(),
                2,
                "every reason must survive the trip — they are what the UI shows"
            );
        }
        other => panic!("client could not read the server's denial: {other:?}\nwire: {wire}"),
    }
}

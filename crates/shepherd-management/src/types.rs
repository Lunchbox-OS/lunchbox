//! Result types returned by [`ManagementService`](crate::ManagementService).

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use shepherd_api::ReasonCode;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum LaunchOutcome {
    Approved {
        session_id: String,
        deadline: Option<DateTime<Local>>,
    },
    Denied {
        reasons: Vec<ReasonCode>,
    },
}

/// The policy file as an editor sees it: its exact bytes, plus an opaque tag
/// that changes when they do.
///
/// The tag exists because a device's policy has three writers — `sudoedit`,
/// `shepherd install policy`, and now the web config editor — and the last one
/// has to be able to notice that it is about to overwrite one of the others.
/// It is a hash rather than an mtime so that a file restored from a backup, or
/// rewritten with identical bytes, reads as unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDocument {
    /// The file's text, exactly as it is on disk — comments, key order and
    /// whitespace included. Returned whether or not it parses: a config the
    /// daemon cannot read is precisely the one an editor is most needed for.
    pub text: String,
    /// Opaque version tag for the bytes above. Compared for equality and
    /// nothing else; the algorithm is not part of the contract.
    pub version: String,
}

impl PolicyDocument {
    /// Tag `text` and take ownership of it.
    pub fn of(text: String) -> Self {
        let version = Self::version_of(&text);
        Self { text, version }
    }

    /// The tag `text` would carry, without building a document around it.
    pub fn version_of(text: &str) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(text.as_bytes());
        // Half a SHA-256 is 128 bits of collision resistance against an
        // adversary who does not exist here: the tag guards against two
        // administrators editing at once, not against a forger.
        digest[..16].iter().map(|b| format!("{b:02x}")).collect()
    }
}

//! Noticing that the policy file changed (issue #157).
//!
//! lunchboxd used to watch its own config directory with inotify and reload on
//! a write. It cannot watch a directory it cannot open, so the watch moves to
//! the side that owns it and the result is pushed over the socket.
//!
//! Watching the **directory** rather than the file, exactly as lunchboxd did:
//! an editor that writes through a temp file and renames — which is what
//! `LocalProtectedFiles::write` does, and what any careful writer does —
//! replaces the inode, and a watch on the file itself would follow the old one
//! into oblivion.

use std::path::Path;

use anyhow::{Context, Result};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use lunchbox_util::ProtectedFile;
use tokio::sync::broadcast;
use tracing::{debug, warn};

/// How many pending notifications a slow watcher may fall behind by.
///
/// Small on purpose: the message carries nothing, so a client that misses three
/// and is told once has lost nothing. `Lagged` is handled as a change rather
/// than an error for the same reason.
const CHANGE_BUFFER: usize = 8;

/// Start watching `dir` for writes to the policy file.
///
/// The returned sender is subscribed to per connection. It is kept alive by the
/// watcher thread the `notify` watcher owns; dropping every receiver does not
/// stop it, which is what lets a client reconnect its watch without the daemon
/// having to rebuild one.
pub fn config_changes(dir: &Path) -> Result<broadcast::Sender<()>> {
    let (tx, _rx) = broadcast::channel(CHANGE_BUFFER);
    let sender = tx.clone();
    let target = dir.join(ProtectedFile::Config.file_name());

    let mut watcher = RecommendedWatcher::new(
        move |result: notify::Result<notify::Event>| {
            let Ok(event) = result else { return };
            let relevant = matches!(
                event.kind,
                notify::EventKind::Modify(_) | notify::EventKind::Create(_)
            );
            if relevant && event.paths.iter().any(|p| p == &target) {
                // Fails only when nobody is watching, which is the normal state
                // between lunchboxd restarts.
                let _ = sender.send(());
            }
        },
        notify::Config::default(),
    )
    .context("creating the policy watcher")?;

    watcher
        .watch(dir, RecursiveMode::NonRecursive)
        .with_context(|| format!("watching {}", dir.display()))?;
    debug!(dir = %dir.display(), "Watching the policy file for changes");

    // The watcher stops when it is dropped, and it has to outlive this
    // function. Leaking it is the honest way to say "for the life of the
    // process": the alternative is threading a handle through every caller to
    // keep something alive that is never legitimately turned off.
    std::mem::forget(watcher);

    Ok(tx)
}

/// Warn once if the policy file is missing at startup.
///
/// Not an error: a device installed but not yet configured has no policy, and
/// refusing to start would make the custodian the reason a fresh install cannot
/// boot. lunchboxd will say something far more useful about a missing config
/// than this daemon can.
pub fn warn_if_no_policy(dir: &Path) {
    let policy = dir.join(ProtectedFile::Config.file_name());
    if !policy.exists() {
        warn!(
            path = %policy.display(),
            "No policy file in the protected directory; lunchboxd will read its own until \
             one is installed there (issue #157)"
        );
    }
}

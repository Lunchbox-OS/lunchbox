//! The protected files, over the socket (issue #157).
//!
//! [`RemoteFiles`] is to `config.toml` and the BLE admin record what
//! [`crate::RemoteStore`] is to the database: the same operations, served by a
//! uid activities do not have instead of read from a path they do.
//!
//! It implements [`ProtectedFiles`], which is defined in `shepherd-util` rather
//! than here on purpose — `shepherd-ble` keeps the admin record and must not
//! have to depend on a wire crate to do it.

use std::path::PathBuf;

use shepherd_util::{ProtectedFile, ProtectedFiles};

use crate::StateRequest;
use crate::client::Transport;

/// Shepherd's protected files, served by the custodian.
pub struct RemoteFiles {
    transport: Transport,
}

impl RemoteFiles {
    /// Connect to the custodian serving `user`'s files.
    pub fn connect(user: &str) -> std::io::Result<Self> {
        Ok(Self {
            transport: Transport::connect_for_user(user)?,
        })
    }

    /// Connect at an explicit path and expected owner. See
    /// [`crate::RemoteStore::connect_at`].
    pub fn connect_at(socket: PathBuf, expect_uid: u32) -> std::io::Result<Self> {
        Ok(Self {
            transport: Transport::connect_at(socket, expect_uid)?,
        })
    }
}

impl ProtectedFiles for RemoteFiles {
    fn read(&self, file: ProtectedFile) -> std::io::Result<Option<String>> {
        self.transport
            .call(&StateRequest::ReadFile { file })
            .map_err(Into::into)
    }

    fn write(&self, file: ProtectedFile, contents: &str) -> std::io::Result<()> {
        self.transport
            .call(&StateRequest::WriteFile {
                file,
                contents: contents.to_string(),
            })
            .map_err(Into::into)
    }

    fn delete(&self, file: ProtectedFile) -> std::io::Result<bool> {
        self.transport
            .call(&StateRequest::DeleteFile { file })
            .map_err(Into::into)
    }

    fn take(&self, file: ProtectedFile) -> std::io::Result<Option<String>> {
        self.transport
            .call(&StateRequest::TakeFile { file })
            .map_err(Into::into)
    }
}

/// Watch the custodian's policy file, calling `on_change` when it is written.
///
/// Replaces the `notify` watcher shepherdd used to run on its own config
/// directory, which it cannot do for a directory it cannot open. The watch is a
/// *second* connection: the request connection stays strictly one-line-in,
/// one-line-out, which is what keeps its client simple.
///
/// The returned handle keeps the watch alive; dropping it ends it. Errors on
/// the connection end the watch rather than retrying, because the caller has a
/// better answer than this function does — shepherdd re-reads its policy on the
/// next reload either way, and a watch that silently stopped is worth a log
/// line at the level that can write one.
pub struct ConfigWatch {
    _thread: std::thread::JoinHandle<()>,
}

impl ConfigWatch {
    /// Start watching. `on_change` runs on the watch's own thread, so it should
    /// signal rather than work — shepherdd sends on a channel.
    pub fn start(
        user: &str,
        on_change: impl Fn() + Send + 'static,
        on_end: impl FnOnce(std::io::Error) + Send + 'static,
    ) -> std::io::Result<Self> {
        let transport = Transport::connect_for_user(user)?;
        let mut stream = transport.into_watch_stream()?;
        let thread = std::thread::spawn(move || {
            loop {
                match stream.next_change() {
                    Ok(()) => on_change(),
                    Err(e) => {
                        on_end(e);
                        return;
                    }
                }
            }
        });
        Ok(Self { _thread: thread })
    }
}

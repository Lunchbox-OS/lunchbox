//! IPC server implementation

use shepherd_api::{ClientInfo, ClientRole, Event, Request, Response};
use shepherd_util::ClientId;
use std::collections::HashMap;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tracing::{debug, error, info, warn};

use crate::{IpcError, IpcResult};

/// Message from client to server
pub enum ServerMessage {
    Request {
        client_id: ClientId,
        request: Request,
    },
    ClientConnected {
        client_id: ClientId,
        info: ClientInfo,
    },
    ClientDisconnected {
        client_id: ClientId,
    },
}

/// Message sent to the writer task for a connected client
enum WriterMessage {
    /// Write this JSON response as-is
    Response(String),
    /// Write this JSON response, then start forwarding broadcast events
    SubscribeResponse(String),
    /// Write this JSON response, then stop forwarding broadcast events
    UnsubscribeResponse(String),
}

/// IPC Server
pub struct IpcServer {
    socket_path: PathBuf,
    /// `(dev, ino)` of the socket file this server bound, so shutdown removes
    /// only the socket it created. Two daemons briefly overlapping — which is
    /// routine in the dev loop, where a session is stopped and restarted in one
    /// breath — otherwise ends with the outgoing one deleting the path the
    /// incoming one has already bound: the new daemon runs on happily while
    /// every client sits in a reconnect loop against a path that no longer
    /// exists. `None` until [`Self::start`] has bound successfully.
    socket_id: Option<(u64, u64)>,
    listener: Option<UnixListener>,
    clients: Arc<RwLock<HashMap<ClientId, ClientHandle>>>,
    event_tx: broadcast::Sender<Event>,
    message_tx: mpsc::UnboundedSender<ServerMessage>,
    message_rx: Arc<Mutex<Option<mpsc::UnboundedReceiver<ServerMessage>>>>,
}

struct ClientHandle {
    info: ClientInfo,
    response_tx: mpsc::UnboundedSender<WriterMessage>,
}

impl IpcServer {
    /// Create a new IPC server
    pub fn new(socket_path: impl AsRef<Path>) -> Self {
        let (event_tx, _) = broadcast::channel(100);
        let (message_tx, message_rx) = mpsc::unbounded_channel();

        Self {
            socket_path: socket_path.as_ref().to_path_buf(),
            socket_id: None,
            listener: None,
            clients: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
            message_tx,
            message_rx: Arc::new(Mutex::new(Some(message_rx))),
        }
    }

    /// Start listening
    pub async fn start(&mut self) -> IpcResult<()> {
        // Remove existing socket if present. Deliberately unconditional: a
        // daemon that crashed leaves its socket behind, and refusing to bind
        // over it would make a crash unrecoverable without manual cleanup.
        if self.socket_path.exists() {
            std::fs::remove_file(&self.socket_path)?;
        }

        // Create parent directory if needed
        if let Some(parent) = self.socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let listener = UnixListener::bind(&self.socket_path)?;

        // Set socket permissions (readable/writable by owner and group)
        if let Err(err) =
            std::fs::set_permissions(&self.socket_path, std::fs::Permissions::from_mode(0o660))
        {
            if err.kind() == std::io::ErrorKind::PermissionDenied {
                warn!(
                    path = %self.socket_path.display(),
                    "Permission denied setting socket permissions; continuing with defaults"
                );
            } else {
                return Err(err.into());
            }
        }

        info!(path = %self.socket_path.display(), "IPC server listening");

        // Remember which file this is, so shutdown can tell it apart from one
        // another daemon may have bound to the same path in the meantime.
        self.socket_id = std::fs::metadata(&self.socket_path)
            .ok()
            .map(|m| (m.dev(), m.ino()));
        self.listener = Some(listener);

        Ok(())
    }

    /// Get receiver for server messages
    pub async fn take_message_receiver(&self) -> Option<mpsc::UnboundedReceiver<ServerMessage>> {
        self.message_rx.lock().await.take()
    }

    /// Accept connections in a loop
    pub async fn run(&self) -> IpcResult<()> {
        let listener = self
            .listener
            .as_ref()
            .ok_or_else(|| IpcError::ServerError("Server not started".into()))?;

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let client_id = ClientId::new();

                    // Get peer credentials
                    let uid = get_peer_uid(&stream);

                    // Determine role based on UID
                    let role = match uid {
                        Some(0) => ClientRole::Admin, // root
                        Some(u) if u == nix::unistd::getuid().as_raw() => ClientRole::Admin,
                        _ => ClientRole::Shell,
                    };

                    let info = ClientInfo::new(role);
                    let info = if let Some(u) = uid {
                        info.with_uid(u)
                    } else {
                        info
                    };

                    info!(client_id = %client_id, uid = ?uid, role = ?role, "Client connected");

                    self.handle_client(stream, client_id, info).await;
                }
                Err(e) => {
                    error!(error = %e, "Failed to accept connection");
                }
            }
        }
    }

    async fn handle_client(&self, stream: UnixStream, client_id: ClientId, info: ClientInfo) {
        let (read_half, write_half) = stream.into_split();
        let (response_tx, mut response_rx) = mpsc::unbounded_channel::<WriterMessage>();

        // Register client
        {
            let mut clients = self.clients.write().await;
            clients.insert(
                client_id.clone(),
                ClientHandle {
                    info: info.clone(),
                    response_tx: response_tx.clone(),
                },
            );
        }

        // Notify of connection
        let _ = self.message_tx.send(ServerMessage::ClientConnected {
            client_id: client_id.clone(),
            info: info.clone(),
        });

        let message_tx = self.message_tx.clone();
        let event_tx = self.event_tx.clone();
        let client_id_clone = client_id.clone();

        // Spawn reader task — forwards raw requests to the daemon; subscription
        // state is managed entirely by the writer task to preserve ordering.
        let _reader_handle = tokio::spawn(async move {
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();

            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => {
                        debug!(client_id = %client_id_clone, "Client disconnected (EOF)");
                        break;
                    }
                    Ok(_) => {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }

                        match serde_json::from_str::<Request>(line) {
                            Ok(request) => {
                                let _ = message_tx.send(ServerMessage::Request {
                                    client_id: client_id_clone.clone(),
                                    request,
                                });
                            }
                            Err(e) => {
                                warn!(
                                    client_id = %client_id_clone,
                                    error = %e,
                                    "Invalid request"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        debug!(client_id = %client_id_clone, error = %e, "Read error");
                        break;
                    }
                }
            }
        });

        // Spawn writer task — serialises responses and events onto the socket.
        // `is_subscribed` is a local flag so event forwarding only begins AFTER
        // the SubscribeEvents response has been flushed; this prevents the client
        // from seeing an event frame where it expects the subscribe response.
        let mut event_rx = event_tx.subscribe();
        let clients_writer = self.clients.clone();
        let client_id_writer = client_id.clone();
        let message_tx_writer = self.message_tx.clone();

        tokio::spawn(async move {
            let mut writer = write_half;
            let mut is_subscribed = false;

            loop {
                tokio::select! {
                    // Handle responses (and subscription state changes)
                    Some(msg) = response_rx.recv() => {
                        let (json, sub_after) = match msg {
                            WriterMessage::Response(s) => (s, None),
                            WriterMessage::SubscribeResponse(s) => (s, Some(true)),
                            WriterMessage::UnsubscribeResponse(s) => (s, Some(false)),
                        };
                        let mut frame = json;
                        frame.push('\n');
                        if let Err(e) = writer.write_all(frame.as_bytes()).await {
                            debug!(client_id = %client_id_writer, error = %e, "Write error");
                            break;
                        }
                        // Apply subscription change only after the response is on the wire
                        if let Some(v) = sub_after {
                            is_subscribed = v;
                        }
                    }

                    // Handle events (only when subscribed)
                    Ok(event) = event_rx.recv() => {
                        if is_subscribed
                            && let Ok(json) = serde_json::to_string(&event)
                        {
                            let mut frame = json;
                            frame.push('\n');
                            if let Err(e) = writer.write_all(frame.as_bytes()).await {
                                debug!(client_id = %client_id_writer, error = %e, "Event write error");
                                break;
                            }
                        }
                    }
                }
            }

            // Notify of disconnection
            let _ = message_tx_writer.send(ServerMessage::ClientDisconnected {
                client_id: client_id_writer.clone(),
            });

            // Remove client
            let mut clients = clients_writer.write().await;
            clients.remove(&client_id_writer);
        });
    }

    /// Send a response to a specific client
    pub async fn send_response(&self, client_id: &ClientId, response: Response) -> IpcResult<()> {
        let json = serde_json::to_string(&response)?;
        let clients = self.clients.read().await;
        if let Some(handle) = clients.get(client_id) {
            handle
                .response_tx
                .send(WriterMessage::Response(json))
                .map_err(|_| IpcError::ConnectionClosed)?;
        }
        Ok(())
    }

    /// Send the SubscribeEvents response to a client and enable event forwarding.
    /// The writer task guarantees that no events are delivered before this response.
    pub async fn send_subscribe_response(
        &self,
        client_id: &ClientId,
        response: Response,
    ) -> IpcResult<()> {
        let json = serde_json::to_string(&response)?;
        let clients = self.clients.read().await;
        if let Some(handle) = clients.get(client_id) {
            handle
                .response_tx
                .send(WriterMessage::SubscribeResponse(json))
                .map_err(|_| IpcError::ConnectionClosed)?;
        }
        Ok(())
    }

    /// Send the UnsubscribeEvents response to a client and disable event forwarding.
    pub async fn send_unsubscribe_response(
        &self,
        client_id: &ClientId,
        response: Response,
    ) -> IpcResult<()> {
        let json = serde_json::to_string(&response)?;
        let clients = self.clients.read().await;
        if let Some(handle) = clients.get(client_id) {
            handle
                .response_tx
                .send(WriterMessage::UnsubscribeResponse(json))
                .map_err(|_| IpcError::ConnectionClosed)?;
        }
        Ok(())
    }

    /// Broadcast an event to all subscribed clients
    pub fn broadcast_event(&self, event: Event) {
        let _ = self.event_tx.send(event);
    }

    /// Get client info
    pub async fn get_client_info(&self, client_id: &ClientId) -> Option<ClientInfo> {
        let clients = self.clients.read().await;
        clients.get(client_id).map(|h| h.info.clone())
    }

    /// Get connected client count
    pub async fn client_count(&self) -> usize {
        self.clients.read().await.len()
    }

    /// Shut the server down, removing the socket file it created.
    ///
    /// Removes the path **only** if it still holds the same file this server
    /// bound. Anything else at that path belongs to another daemon that has
    /// since taken over, and deleting it would strand every one of its clients
    /// on a path that no longer exists while it went on serving a socket nobody
    /// could reach.
    pub fn shutdown(&self) {
        let Some(bound) = self.socket_id else {
            return; // never started, or the bind was never observed
        };
        match std::fs::metadata(&self.socket_path) {
            Ok(md) if (md.dev(), md.ino()) == bound => {
                let _ = std::fs::remove_file(&self.socket_path);
            }
            Ok(_) => warn!(
                path = %self.socket_path.display(),
                "Another daemon has bound our socket path; leaving it alone"
            ),
            // Already gone: nothing to clean up.
            Err(_) => {}
        }
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Get peer UID from Unix socket
fn get_peer_uid(stream: &UnixStream) -> Option<u32> {
    use std::os::unix::io::AsFd;

    // Get the borrowed file descriptor from the stream
    let fd = stream.as_fd();

    match nix::sys::socket::getsockopt(&fd, nix::sys::socket::sockopt::PeerCredentials) {
        Ok(cred) => Some(cred.uid()),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_server_start() {
        let dir = tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");

        let mut server = IpcServer::new(&socket_path);
        if let Err(err) = server.start().await {
            if let IpcError::Io(ref io_err) = err
                && io_err.kind() == std::io::ErrorKind::PermissionDenied
            {
                eprintln!(
                    "Skipping IPC server start test due to permission error: {}",
                    io_err
                );
                return;
            }
            panic!("IPC server start failed: {err}");
        }

        assert!(socket_path.exists());
    }

    /// The dev loop's overlap, in miniature: one daemon is still shutting down
    /// while the next has already taken the socket path. The outgoing one used
    /// to delete it, leaving the incoming daemon healthy but unreachable — the
    /// session comes up, and every client loops on `No such file or directory`.
    #[tokio::test]
    async fn shutdown_does_not_delete_a_socket_another_daemon_has_bound() {
        let dir = tempdir().unwrap();
        let socket_path = dir.path().join("shepherd.sock");

        let mut outgoing = IpcServer::new(&socket_path);
        if outgoing.start().await.is_err() {
            return; // sandbox without permission to bind; covered by test_server_start
        }

        // The incoming daemon replaces the path, exactly as `start` does.
        let mut incoming = IpcServer::new(&socket_path);
        incoming.start().await.unwrap();
        let taken_over = std::fs::metadata(&socket_path).unwrap().ino();

        outgoing.shutdown();

        assert!(
            socket_path.exists(),
            "the outgoing daemon deleted the incoming daemon's socket"
        );
        assert_eq!(
            std::fs::metadata(&socket_path).unwrap().ino(),
            taken_over,
            "the socket at the path is still the one the incoming daemon bound"
        );

        // And the daemon that does own it still cleans up after itself.
        incoming.shutdown();
        assert!(!socket_path.exists());
    }

    #[tokio::test]
    async fn shutdown_before_start_removes_nothing() {
        let dir = tempdir().unwrap();
        let socket_path = dir.path().join("shepherd.sock");
        std::fs::write(&socket_path, b"not ours").unwrap();

        // A server that never bound has no claim on the path, so it must not
        // touch whatever happens to be sitting there.
        IpcServer::new(&socket_path).shutdown();

        assert!(socket_path.exists());
    }
}

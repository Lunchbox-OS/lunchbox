//! IPC server implementation

use lunchbox_api::{ClientInfo, Event, Request, Response};
use lunchbox_util::ClientId;
use std::collections::HashMap;
use std::os::fd::AsFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tracing::{debug, error, info, warn};

use crate::peer::{PeerPolicy, Rejection};
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
    /// A peer was refused at accept (issue #144). Reported rather than only
    /// logged: something at this uid tried to drive the daemon from outside
    /// the session, which is an administrator-facing condition, not a debug
    /// detail.
    ClientRejected {
        rejection: Rejection,
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
    /// `(st_dev, st_ino)` of the socket this server bound, for [`Self::socket_was_replaced`].
    bound_identity: Option<(u64, u64)>,
    clients: Arc<RwLock<HashMap<ClientId, ClientHandle>>>,
    event_tx: broadcast::Sender<Event>,
    message_tx: mpsc::UnboundedSender<ServerMessage>,
    message_rx: Arc<Mutex<Option<mpsc::UnboundedReceiver<ServerMessage>>>>,
    /// Which peers may connect (issue #144). Unrestricted unless the daemon
    /// arms it, so a caller that never opts in behaves as it always did.
    peer_policy: PeerPolicy,
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
            bound_identity: None,
            clients: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
            message_tx,
            message_rx: Arc::new(Mutex::new(Some(message_rx))),
            peer_policy: PeerPolicy::unrestricted(),
        }
    }

    /// Whether the socket at our path is no longer the one we bound.
    ///
    /// `true` means something replaced or removed it — an activity can, since
    /// it shares this uid and no file mode prevents it (see
    /// [`crate::ServerCheck`]). Clients refuse to talk to the impostor, so this
    /// is not a breach; it is the daemon becoming unreachable, which is worth
    /// saying out loud rather than leaving as a launcher that mysteriously
    /// stops working.
    ///
    /// `false` when we never bound, or when the path cannot be read — an
    /// unreadable path is not evidence of replacement.
    pub fn socket_was_replaced(&self) -> bool {
        let Some(bound) = self.bound_identity else {
            return false;
        };
        match socket_identity(&self.socket_path) {
            Some(now) => now != bound,
            // Gone entirely. The unlink half of the same act.
            None => true,
        }
    }

    /// Restrict which peers this server will accept (issue #144).
    ///
    /// Must be called before [`Self::run`]; the policy is consulted once per
    /// connection at accept.
    pub fn set_peer_policy(&mut self, policy: PeerPolicy) {
        self.peer_policy = policy;
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

        // Deliberately a filesystem socket, and it must stay one (issue #144).
        //
        // An abstract socket would be tempting: it has no directory entry, so
        // the takeover this file guards against with `socket_was_replaced`
        // would be impossible. But an abstract name ignores filesystem
        // permissions entirely, and that forecloses the fix that actually ends
        // this whole class — separating lunchbox's uid from the activities'
        // (#105/#157), after which a 0700 socket directory does the job that no
        // amount of peer checking can do while the uid is shared.
        let listener = UnixListener::bind(&self.socket_path)?;

        // Remember which file we bound, so a replacement can be noticed
        // (issue #144). An activity shares this uid and so can `unlink()` the
        // socket and bind its own at the same path; clients refuse to talk to
        // the impostor, but the daemon would otherwise never learn that it had
        // become unreachable.
        self.bound_identity = socket_identity(&self.socket_path);

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

        let mut rejections = RejectionReporter::default();

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let client_id = ClientId::new();

                    // Get peer credentials
                    let uid = get_peer_uid(&stream);

                    // Decide here, at accept, rather than at dispatch: one
                    // decision per connection instead of one per call, it
                    // cannot be forgotten when a method is added, and a peer
                    // that should not read state at all never reaches the
                    // event stream (issue #144).
                    let role = match self.peer_policy.classify(stream.as_fd(), uid) {
                        Ok(role) => role,
                        Err(rejection) => {
                            // Reported at most once a minute: a refused peer can
                            // reconnect as fast as the kernel allows, and each
                            // report wakes every diagnostics subscriber.
                            if let Some(suppressed) = rejections.should_report(Instant::now()) {
                                warn!(
                                    client_id = %client_id,
                                    uid = ?uid,
                                    peer_pid = ?rejection.peer_pid,
                                    peer_cgroup = ?rejection.peer_cgroup,
                                    reason = %rejection.reason,
                                    suppressed_since_last_report = suppressed,
                                    "Refused a client on the management socket"
                                );
                                let _ = self
                                    .message_tx
                                    .send(ServerMessage::ClientRejected { rejection });
                            }
                            // Dropping the stream closes the connection. The
                            // peer sees EOF rather than an error frame: there
                            // is nothing useful to tell it, and a refusal that
                            // answers is a refusal that can be probed.
                            continue;
                        }
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
/// How long after reporting a refused peer before another is reported.
///
/// The first refusal is always reported, so a single probe is never silent.
const REJECTION_REPORT_INTERVAL: Duration = Duration::from_secs(60);

/// Rate-limits reporting of refused peers (issue #144).
///
/// A refusal costs more than the connection that caused it: a `warn!` line, and
/// a `ClientRejected` message that becomes a diagnostic — and raising a
/// diagnostic whose text has changed wakes every subscriber, which is the web
/// UI, the companion app and the launcher. The peer's cgroup is deliberately
/// part of that text (it is what turns "something probed the socket" into
/// "this activity did"), so every refusal is a distinct diagnostic and every
/// one would broadcast.
///
/// An activity can call `connect()` in a loop. Nothing is breached — it is
/// refused every time — but it would be noise it controls, aimed squarely at
/// the channel an administrator watches for exactly this warning. So the first
/// refusal is reported in full and the rest are counted, with the tally carried
/// on the next report.
#[derive(Debug, Default)]
struct RejectionReporter {
    last_report: Option<Instant>,
    suppressed: u64,
}

impl RejectionReporter {
    /// `Some(suppressed_since_last_report)` when this refusal should be
    /// reported, `None` when it should only be counted.
    fn should_report(&mut self, now: Instant) -> Option<u64> {
        let due = match self.last_report {
            None => true,
            Some(last) => now.duration_since(last) >= REJECTION_REPORT_INTERVAL,
        };
        if due {
            self.last_report = Some(now);
            Some(std::mem::take(&mut self.suppressed))
        } else {
            self.suppressed += 1;
            None
        }
    }
}

/// `(st_dev, st_ino)` for the socket at `path`, or `None` if it cannot be read.
fn socket_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

fn get_peer_uid(stream: &UnixStream) -> Option<u32> {
    use std::os::unix::io::AsFd;
    crate::peer::peer_uid(stream.as_fd())
}

#[cfg(test)]
mod tests {
    use super::{REJECTION_REPORT_INTERVAL, RejectionReporter};
    use std::time::Instant;

    /// A single probe must never be silent — that is the whole point of the
    /// warning — while a peer that reconnects in a loop must not get to wake
    /// every diagnostics subscriber each time (issue #144).
    #[test]
    fn the_first_refusal_is_reported_and_a_flood_is_counted() {
        let mut r = RejectionReporter::default();
        let t0 = Instant::now();

        assert_eq!(
            r.should_report(t0),
            Some(0),
            "the first refusal must always be reported"
        );

        for _ in 0..10_000 {
            assert_eq!(
                r.should_report(t0),
                None,
                "a flood inside the window must be counted, not reported"
            );
        }

        // The tally rides along on the next report, so the flood is visible
        // without having been broadcast ten thousand times.
        assert_eq!(
            r.should_report(t0 + REJECTION_REPORT_INTERVAL),
            Some(10_000)
        );
        // ...and resets once carried.
        assert_eq!(r.should_report(t0 + REJECTION_REPORT_INTERVAL * 2), Some(0));
    }

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
        let socket_path = dir.path().join("lunchbox.sock");

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
        let socket_path = dir.path().join("lunchbox.sock");
        std::fs::write(&socket_path, b"not ours").unwrap();

        // A server that never bound has no claim on the path, so it must not
        // touch whatever happens to be sitting there.
        IpcServer::new(&socket_path).shutdown();

        assert!(socket_path.exists());
    }
}

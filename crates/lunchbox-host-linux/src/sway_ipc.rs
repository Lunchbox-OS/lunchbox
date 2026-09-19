//! A client for sway's IPC socket (`sway-ipc(7)`).
//!
//! Replaces the `swaymsg` subprocess every compositor call used to spawn
//! (issue #147). The tree query is on the supervision path now — the escape and
//! orphan sweeps both read it — and a `fork`/`exec` per query cost roughly
//! 0.85ms of CPU against 0.03ms for a round trip on a held connection.
//!
//! **The protocol.** A unix socket whose path is in `$SWAYSOCK` (or `$I3SOCK`),
//! carrying a 14-byte header — the magic string `i3-ipc`, a `u32` payload
//! length and a `u32` message type, both in *native* byte order — followed by a
//! JSON payload. Replies use the same framing and echo the message type back;
//! events set the high bit ([`EVENT_BIT`]).
//!
//! **There is no socket discovery beyond the environment.** `swaymsg` has no
//! `--get-socketpath`; `sway --get-socketpath` only echoes `$SWAYSOCK` back and
//! reports "sway socket not detected" when it is unset. So a missing variable
//! is a hard error rather than something to search around, and it surfaces as
//! [`lunchbox_api::DiagnosticCode::CompositorUnreachable`] instead of looking
//! like an empty screen.
//!
//! **Losing an established connection is terminal.** The daemon is `exec`'d by
//! sway and dies with it, so a dropped connection means the session is over
//! rather than that the compositor will be back. Reconnecting would also be
//! incompatible with [`unlink_socket`], which deliberately destroys the only
//! path a reconnect could use. A connection that has never been established is
//! retried, so one transient error during startup does not disable the
//! compositor for the whole session.

use lunchbox_host_api::{HostError, HostResult};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

/// Leading bytes of every message and reply, for i3 compatibility.
const MAGIC: &[u8; 6] = b"i3-ipc";
/// `MAGIC` + `u32` length + `u32` type.
const HEADER_LEN: usize = 14;

/// A reply larger than this is treated as a framing error rather than
/// allocated. A `get_tree` on a loaded kiosk is tens of kilobytes; this is
/// four orders of magnitude of headroom and exists only so a desynchronised
/// stream cannot ask us for an arbitrary allocation.
const MAX_PAYLOAD: u32 = 64 * 1024 * 1024;

// Message types, per sway-ipc(7). Only the four we need.
pub const RUN_COMMAND: u32 = 0;
pub const SUBSCRIBE: u32 = 2;
pub const GET_OUTPUTS: u32 = 3;
pub const GET_TREE: u32 = 4;

/// Set on the payload type of every event, distinguishing it from a reply.
pub const EVENT_BIT: u32 = 0x8000_0000;
/// `output` event — a display was connected, disconnected or reconfigured.
pub const EVENT_OUTPUT: u32 = 0x8000_0001;
/// `window` event — a surface was created, closed, moved, focused or retitled.
pub const EVENT_WINDOW: u32 = 0x8000_0003;

/// Sway's reply to a `SUBSCRIBE`.
#[derive(Debug, Deserialize)]
struct SubscribeReply {
    success: bool,
}

/// The socket path from the environment.
///
/// `$SWAYSOCK` first, then `$I3SOCK` — the same order `swaymsg` uses, and the
/// whole of it. See the module docs on why there is no fallback.
pub fn socket_path() -> HostResult<PathBuf> {
    for var in ["SWAYSOCK", "I3SOCK"] {
        if let Ok(v) = std::env::var(var)
            && !v.is_empty()
        {
            return Ok(PathBuf::from(v));
        }
    }
    Err(HostError::Internal(
        "neither SWAYSOCK nor I3SOCK is set, so the sway IPC socket cannot be found".into(),
    ))
}

/// One connection to sway. Sway answers requests in order on a connection, so
/// a caller holding this exclusively can send and then read the reply.
pub struct Connection {
    stream: UnixStream,
}

impl Connection {
    /// Connect to the socket named by the environment.
    pub async fn open() -> HostResult<Self> {
        Self::open_at(&socket_path()?).await
    }

    /// Connect to a specific socket. Used by the tests, which stand up a fake
    /// sway on a temporary path.
    pub async fn open_at(path: &Path) -> HostResult<Self> {
        let stream = UnixStream::connect(path).await.map_err(|e| {
            HostError::Internal(format!(
                "cannot connect to the sway IPC socket at {}: {e}",
                path.display()
            ))
        })?;
        Ok(Self { stream })
    }

    /// Frame and write one message.
    async fn send(&mut self, message_type: u32, payload: &[u8]) -> HostResult<()> {
        let len = u32::try_from(payload.len())
            .map_err(|_| HostError::Internal("sway IPC payload is too large".into()))?;
        let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
        frame.extend_from_slice(MAGIC);
        // Native byte order is what the protocol specifies, not an oversight.
        frame.extend_from_slice(&len.to_ne_bytes());
        frame.extend_from_slice(&message_type.to_ne_bytes());
        frame.extend_from_slice(payload);
        self.stream
            .write_all(&frame)
            .await
            .map_err(|e| HostError::Internal(format!("writing to the sway IPC socket: {e}")))
    }

    /// Read one reply or event, returning its payload type and body.
    async fn recv(&mut self) -> HostResult<(u32, Vec<u8>)> {
        let mut header = [0u8; HEADER_LEN];
        self.stream
            .read_exact(&mut header)
            .await
            .map_err(|e| HostError::Internal(format!("reading from the sway IPC socket: {e}")))?;
        if &header[..6] != MAGIC {
            return Err(HostError::Internal(
                "sway IPC reply did not start with the i3-ipc magic string".into(),
            ));
        }
        let len = u32::from_ne_bytes(header[6..10].try_into().expect("4 bytes"));
        let payload_type = u32::from_ne_bytes(header[10..14].try_into().expect("4 bytes"));
        if len > MAX_PAYLOAD {
            return Err(HostError::Internal(format!(
                "sway IPC reply claims {len} bytes, which is beyond anything sway sends"
            )));
        }
        let mut payload = vec![0u8; len as usize];
        self.stream
            .read_exact(&mut payload)
            .await
            .map_err(|e| HostError::Internal(format!("reading a sway IPC payload: {e}")))?;
        Ok((payload_type, payload))
    }

    /// Send a request and read its reply. Only valid on a connection with no
    /// subscription, where nothing else can arrive in between.
    pub async fn request(&mut self, message_type: u32, payload: &[u8]) -> HostResult<Vec<u8>> {
        self.send(message_type, payload).await?;
        let (reply_type, body) = self.recv().await?;
        if reply_type != message_type {
            return Err(HostError::Internal(format!(
                "sway answered a type {message_type} request with a type {reply_type} reply"
            )));
        }
        Ok(body)
    }
}

/// A connection subscribed to sway events.
///
/// Its own connection rather than a shared one: sway will answer requests on a
/// subscribed connection, but only a dedicated one lets a reader block on the
/// next event without having to demultiplex replies out of the same stream.
pub struct Subscription {
    conn: Connection,
}

impl Subscription {
    /// Subscribe to the named event types, e.g. `["window"]`.
    pub async fn open(events: &[&str]) -> HostResult<Self> {
        Self::open_at(&socket_path()?, events).await
    }

    pub async fn open_at(path: &Path, events: &[&str]) -> HostResult<Self> {
        let mut conn = Connection::open_at(path).await?;
        let payload = serde_json::to_vec(events)
            .map_err(|e| HostError::Internal(format!("encoding a sway subscription: {e}")))?;
        let body = conn.request(SUBSCRIBE, &payload).await?;
        let reply: SubscribeReply = serde_json::from_slice(&body)
            .map_err(|e| HostError::Internal(format!("parsing sway's subscription reply: {e}")))?;
        if !reply.success {
            return Err(HostError::Internal(format!(
                "sway refused a subscription to {events:?}"
            )));
        }
        Ok(Self { conn })
    }

    /// Block until the next event. `Err` means the stream is over — sway exited,
    /// or the connection broke — which for us means the session is ending.
    pub async fn next_event(&mut self) -> HostResult<(u32, Vec<u8>)> {
        let (payload_type, body) = self.conn.recv().await?;
        Ok((payload_type, body))
    }
}

/// Whether the shared connection has been established, and whether it is still
/// usable.
enum State {
    /// Not connected yet. A connect failure leaves us here, so a transient
    /// error during startup is retried rather than disabling the session.
    Fresh,
    Up(Connection),
    /// An established connection failed. Terminal — see the module docs.
    Down(String),
}

/// The process-wide request connection.
///
/// One connection behind a mutex, because sway serialises replies per
/// connection: holding it across send-then-receive is what makes a reply
/// unambiguously ours.
pub struct SwayIpc {
    state: Mutex<State>,
}

static CLIENT: OnceLock<SwayIpc> = OnceLock::new();

/// The shared client. Connects on first use.
pub fn client() -> &'static SwayIpc {
    CLIENT.get_or_init(|| SwayIpc {
        state: Mutex::new(State::Fresh),
    })
}

impl SwayIpc {
    /// Send a request on the shared connection, connecting if needed.
    pub async fn request(&self, message_type: u32, payload: &[u8]) -> HostResult<Vec<u8>> {
        let mut state = self.state.lock().await;

        if let State::Down(why) = &*state {
            return Err(HostError::Internal(format!(
                "the sway IPC connection was lost and is not re-established: {why}"
            )));
        }

        if matches!(*state, State::Fresh) {
            // A failure here leaves us Fresh on purpose: nothing was lost, so
            // the next caller may try again.
            *state = State::Up(Connection::open().await?);
        }

        let State::Up(conn) = &mut *state else {
            unreachable!("state is Up immediately above");
        };
        match conn.request(message_type, payload).await {
            Ok(body) => Ok(body),
            Err(e) => {
                // An established connection that fails does not come back.
                *state = State::Down(e.to_string());
                Err(e)
            }
        }
    }

    /// Like [`Self::request`], but a connection that dies mid-request yields
    /// `Ok(None)` instead of an error.
    ///
    /// Exists for `exit`, the one command whose success is indistinguishable
    /// from a transport failure: sway may tear the socket down before it
    /// replies. Everywhere else a dead connection is a real error, so this is
    /// deliberately not the default.
    pub async fn request_tolerating_disconnect(
        &self,
        message_type: u32,
        payload: &[u8],
    ) -> HostResult<Option<Vec<u8>>> {
        let mut state = self.state.lock().await;

        if matches!(*state, State::Down(_)) {
            return Ok(None);
        }
        if matches!(*state, State::Fresh) {
            match Connection::open().await {
                Ok(conn) => *state = State::Up(conn),
                Err(_) => return Ok(None),
            }
        }

        let State::Up(conn) = &mut *state else {
            unreachable!("state is Up immediately above");
        };
        match conn.request(message_type, payload).await {
            Ok(body) => Ok(Some(body)),
            Err(e) => {
                *state = State::Down(e.to_string());
                Ok(None)
            }
        }
    }

    /// Establish the connection now rather than on first use, so that startup
    /// can report an unreachable compositor before anything depends on it —
    /// and so [`unlink_socket`] has something to keep alive.
    pub async fn connect_now(&self) -> HostResult<()> {
        let mut state = self.state.lock().await;
        match &*state {
            State::Down(why) => Err(HostError::Internal(format!(
                "the sway IPC connection was lost and is not re-established: {why}"
            ))),
            State::Up(_) => Ok(()),
            State::Fresh => {
                *state = State::Up(Connection::open().await?);
                Ok(())
            }
        }
    }
}

/// Give the sway IPC socket a second name, so something can still reach the
/// compositor after [`unlink_socket`] removes the ambient one.
///
/// A hard link rather than a copy or a symlink: connecting resolves a path to
/// the socket's inode, and the inode stays alive while any name points at it.
/// That means `alias` has to be on the same filesystem as the socket, i.e.
/// inside `$XDG_RUNTIME_DIR`.
///
/// Idempotent, because a second lunchboxd in the same session would otherwise
/// fail on `EEXIST` at startup. It deliberately never *removes* an alias — the
/// first daemon's callers are still using it.
pub fn alias_socket(alias: &Path) -> HostResult<()> {
    let socket = socket_path()?;
    match std::fs::hard_link(&socket, alias) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(HostError::Internal(format!(
            "cannot link the sway IPC socket {} to {}: {e}",
            socket.display(),
            alias.display()
        ))),
    }
}

/// Remove the sway IPC socket's name from the filesystem.
///
/// Sway keeps its listening socket open, and connections already established
/// keep working — but nothing can connect by path afterwards, which takes the
/// compositor's command channel away from every other process running as this
/// uid (issue #144). That matters because `RUN_COMMAND "exec …"` starts a
/// process outside lunchbox's supervision *and* outside the cgroup the
/// per-entry firewall is attached to.
///
/// Destructive and irreversible: an unlinked socket with no other name cannot
/// be recovered, and it belongs to whatever sway session lunchboxd is inside —
/// run by hand in a developer's own desktop, this takes that desktop's socket
/// away from every other client. Hardening is on by default, so the guard is
/// `--no-harden-sway-ipc` on every development entry point rather than a flag
/// production remembers to set. Call it only once every connection this daemon
/// needs is established, and only after [`alias_socket`] has succeeded if an
/// alias was requested.
pub fn unlink_socket() -> HostResult<()> {
    let socket = socket_path()?;
    std::fs::remove_file(&socket).map_err(|e| {
        HostError::Internal(format!(
            "cannot unlink the sway IPC socket {}: {e}",
            socket.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    /// `set_var`/`remove_var` are process-global, so the two tests that touch
    /// the environment must not run concurrently with each other. Cargo runs
    /// a crate's tests on parallel threads in one process, so without this
    /// they interleave and both flake.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Take the environment lock, tolerating a poisoned mutex — a panicking
    /// test elsewhere should fail on its own terms, not by poisoning this one.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Read one framed message from a stream, as sway would.
    async fn read_frame(stream: &mut UnixStream) -> (u32, Vec<u8>) {
        let mut header = [0u8; HEADER_LEN];
        stream.read_exact(&mut header).await.unwrap();
        assert_eq!(&header[..6], MAGIC);
        let len = u32::from_ne_bytes(header[6..10].try_into().unwrap());
        let ty = u32::from_ne_bytes(header[10..14].try_into().unwrap());
        let mut payload = vec![0u8; len as usize];
        stream.read_exact(&mut payload).await.unwrap();
        (ty, payload)
    }

    /// Write one framed reply or event, as sway would.
    async fn write_frame(stream: &mut UnixStream, payload_type: u32, body: &[u8]) {
        let mut frame = Vec::new();
        frame.extend_from_slice(MAGIC);
        frame.extend_from_slice(&(body.len() as u32).to_ne_bytes());
        frame.extend_from_slice(&payload_type.to_ne_bytes());
        frame.extend_from_slice(body);
        stream.write_all(&frame).await.unwrap();
    }

    /// A socket in a temp dir plus the listener bound to it. The `TempDir` is
    /// returned so the caller keeps it alive for the length of the test.
    fn fake_sway() -> (tempfile::TempDir, PathBuf, UnixListener) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sway.sock");
        let listener = UnixListener::bind(&path).unwrap();
        (dir, path, listener)
    }

    #[tokio::test]
    async fn round_trips_a_request() {
        let (_dir, path, listener) = fake_sway();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (ty, payload) = read_frame(&mut stream).await;
            assert_eq!(ty, GET_TREE);
            assert!(payload.is_empty());
            write_frame(&mut stream, GET_TREE, br#"{"id":1}"#).await;
        });

        let mut conn = Connection::open_at(&path).await.unwrap();
        let body = conn.request(GET_TREE, b"").await.unwrap();
        assert_eq!(body, br#"{"id":1}"#);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn carries_the_command_payload_and_returns_the_reply() {
        let (_dir, path, listener) = fake_sway();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (ty, payload) = read_frame(&mut stream).await;
            assert_eq!(ty, RUN_COMMAND);
            assert_eq!(payload, b"output eDP-1 scale 1.5");
            write_frame(&mut stream, RUN_COMMAND, br#"[{"success":true}]"#).await;
        });

        let mut conn = Connection::open_at(&path).await.unwrap();
        let body = conn
            .request(RUN_COMMAND, b"output eDP-1 scale 1.5")
            .await
            .unwrap();
        assert_eq!(body, br#"[{"success":true}]"#);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn rejects_a_reply_of_the_wrong_type() {
        // A desynchronised stream must not be parsed as if it answered the
        // question we asked.
        let (_dir, path, listener) = fake_sway();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = read_frame(&mut stream).await;
            write_frame(&mut stream, GET_OUTPUTS, b"[]").await;
        });

        let mut conn = Connection::open_at(&path).await.unwrap();
        let err = conn.request(GET_TREE, b"").await.unwrap_err();
        assert!(
            err.to_string().contains("type 3 reply"),
            "unexpected error: {err}"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn rejects_a_reply_without_the_magic_string() {
        let (_dir, path, listener) = fake_sway();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = read_frame(&mut stream).await;
            stream.write_all(&[0u8; HEADER_LEN]).await.unwrap();
        });

        let mut conn = Connection::open_at(&path).await.unwrap();
        let err = conn.request(GET_TREE, b"").await.unwrap_err();
        assert!(
            err.to_string().contains("magic string"),
            "unexpected error: {err}"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_broken_connection_is_an_error_not_an_empty_reply() {
        // The defect this whole client exists to fix: a failed query must be
        // distinguishable from "nothing on screen".
        let (_dir, path, listener) = fake_sway();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            drop(stream); // sway went away mid-request
        });

        let mut conn = Connection::open_at(&path).await.unwrap();
        assert!(conn.request(GET_TREE, b"").await.is_err());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn subscribes_and_reads_events() {
        let (_dir, path, listener) = fake_sway();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (ty, payload) = read_frame(&mut stream).await;
            assert_eq!(ty, SUBSCRIBE);
            assert_eq!(payload, br#"["window"]"#);
            write_frame(&mut stream, SUBSCRIBE, br#"{"success":true}"#).await;
            write_frame(&mut stream, EVENT_WINDOW, br#"{"change":"new"}"#).await;
        });

        let mut sub = Subscription::open_at(&path, &["window"]).await.unwrap();
        let (ty, body) = sub.next_event().await.unwrap();
        assert_eq!(ty, EVENT_WINDOW);
        assert_eq!(body, br#"{"change":"new"}"#);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_refused_subscription_is_an_error() {
        let (_dir, path, listener) = fake_sway();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = read_frame(&mut stream).await;
            write_frame(&mut stream, SUBSCRIBE, br#"{"success":false}"#).await;
        });

        assert!(Subscription::open_at(&path, &["window"]).await.is_err());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn aliasing_is_idempotent_and_the_alias_outlives_the_unlink() {
        // The property the #144 hardening rests on: a hard link keeps the
        // socket reachable after its ambient name is gone, and a second
        // daemon re-running the same setup must not fail or break the first.
        let (_dir, path, listener) = fake_sway();
        let alias = path.with_file_name("alias.sock");

        // Scoped so the guards are dropped before the assertions below.
        {
            let _env = env_lock();
            let _guard = EnvGuard::set("SWAYSOCK", path.to_str().unwrap());
            alias_socket(&alias).unwrap();
            alias_socket(&alias).expect("a repeated alias must not fail");
            unlink_socket().unwrap();
            assert!(!path.exists(), "the ambient name should be gone");
        }

        // Still connectable through the alias, which is the whole point.
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = read_frame(&mut stream).await;
            write_frame(&mut stream, GET_TREE, b"{}").await;
        });
        let mut conn = Connection::open_at(&alias).await.unwrap();
        assert_eq!(conn.request(GET_TREE, b"").await.unwrap(), b"{}");
        server.await.unwrap();
    }

    #[test]
    fn socket_path_prefers_swaysock_then_i3sock_then_fails() {
        let _env = env_lock();
        let _swaysock = EnvGuard::unset("SWAYSOCK");
        let _i3sock = EnvGuard::unset("I3SOCK");
        assert!(socket_path().is_err());

        let _i3 = EnvGuard::set("I3SOCK", "/tmp/i3.sock");
        assert_eq!(socket_path().unwrap(), PathBuf::from("/tmp/i3.sock"));

        let _sway = EnvGuard::set("SWAYSOCK", "/tmp/sway.sock");
        assert_eq!(socket_path().unwrap(), PathBuf::from("/tmp/sway.sock"));
    }

    /// Set or clear an env var for the length of a test and put it back after.
    /// Always paired with [`env_lock`].
    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }

        fn unset(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe { std::env::remove_var(key) };
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(v) => unsafe { std::env::set_var(self.key, v) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }
}

//! Both halves of the wire against each other, over a real socket (issue #157).
//!
//! `RemoteStore` has to behave like the `SqliteStore` it replaces, or the
//! engine's arithmetic changes when a device is protected — which would be the
//! worst possible way to find a bug in this. So the test drives the *client*
//! trait methods and asserts on what the *server's* store actually holds.
//!
//! The server here is the same `server::handle` the daemon uses, over the same
//! NDJSON framing; what it does not exercise is the peer check, which needs
//! cgroups and a session and is covered in `lunchbox-ipc` and on a device.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Local, NaiveDate};
use lunchbox_state_proto::{RemoteStore, StateRequest, server};
use lunchbox_store::{
    AuditEvent, AuditEventType, SqliteStore, StateSnapshot, Store, StoreError, TokenState,
};
use lunchbox_util::{EntryId, LimitSubject};

/// Run the custodian's side of the wire on `socket`, backed by `store`.
///
/// A thread per connection, like the daemon's task per connection. That is not
/// incidental: a client holds a *store* connection open for the life of the
/// session and opens a second for the protected files, so a server that
/// finished one connection before accepting the next would deadlock the moment
/// anything used both.
fn spawn_server(
    socket: PathBuf,
    store: Arc<dyn Store>,
    files: Arc<dyn lunchbox_util::ProtectedFiles>,
) -> std::thread::JoinHandle<()> {
    let listener = UnixListener::bind(&socket).expect("bind");
    std::thread::spawn(move || {
        while let Ok((stream, _)) = listener.accept() {
            let store = Arc::clone(&store);
            let files = Arc::clone(&files);
            std::thread::spawn(move || {
                let mut writer = stream.try_clone().expect("clone");
                let reader = BufReader::new(stream);
                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    let request: StateRequest = match serde_json::from_str(&line) {
                        Ok(r) => r,
                        Err(_) => break,
                    };
                    let reply = server::handle(store.as_ref(), files.as_ref(), request);
                    if writer.write_all(reply.as_bytes()).is_err()
                        || writer.write_all(b"\n").is_err()
                        || writer.flush().is_err()
                    {
                        break;
                    }
                }
            });
        }
    })
}

struct Fixture {
    dir: tempfile::TempDir,
    socket: PathBuf,
    remote: RemoteStore,
    direct: Arc<dyn Store>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("lunchboxd.db");
    let socket = dir.path().join("state.sock");
    let direct: Arc<dyn Store> = Arc::new(SqliteStore::open(&db).expect("open"));
    let files: Arc<dyn lunchbox_util::ProtectedFiles> = Arc::new(
        lunchbox_util::LocalProtectedFiles::new(dir.path().to_path_buf()),
    );
    spawn_server(socket.clone(), Arc::clone(&direct), files);

    // Both halves run as this test's own uid, so that is what the client is
    // told to expect. The check is the same one a device makes; only the uid it
    // is held to differs, which is why `connect_at` takes it rather than
    // assuming.
    let remote =
        RemoteStore::connect_at(socket.clone(), nix::unistd::getuid().as_raw()).expect("connect");
    Fixture {
        dir,
        socket,
        remote,
        direct,
    }
}

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 2).expect("date")
}

#[test]
fn usage_written_through_the_wire_is_what_the_database_holds() {
    let f = fixture();
    let entry = EntryId::new("tuxmath");

    assert_eq!(
        f.remote.get_usage(&entry, day()).expect("get"),
        Duration::ZERO
    );

    f.remote
        .add_usage(&entry, day(), Duration::from_secs(90))
        .expect("add");
    f.remote
        .add_usage(&entry, day(), Duration::from_secs(30))
        .expect("add again");

    // The assertion that matters: not that the client echoes itself back, but
    // that the *server's* store agrees.
    assert_eq!(
        f.direct.get_usage(&entry, day()).expect("direct"),
        Duration::from_secs(120)
    );
    assert_eq!(
        f.remote.get_usage(&entry, day()).expect("remote"),
        Duration::from_secs(120)
    );
}

#[test]
fn tokens_cooldowns_and_overrides_survive_the_wire() {
    let f = fixture();
    let subject = LimitSubject::entry("minecraft");

    let state = f
        .remote
        .adjust_token_balance(&subject, day(), false, 600)
        .expect("adjust");
    assert_eq!(state.balance, Duration::from_secs(600));
    let after: TokenState = f
        .remote
        .get_token_state(&subject, day(), false)
        .expect("get tokens");
    assert_eq!(after.balance, Duration::from_secs(600));

    let until = lunchbox_util::now() + chrono::Duration::minutes(5);
    f.remote
        .set_cooldown_until(&subject, until)
        .expect("cooldown");
    let read = f
        .remote
        .get_cooldown_until(&subject)
        .expect("get cooldown")
        .expect("some");
    // Through JSON and SQLite, so compare at the resolution the store keeps
    // rather than demanding bit-identical instants.
    assert!((read - until).num_seconds().abs() <= 1);
    f.remote.clear_cooldown(&subject).expect("clear");
    assert!(
        f.remote
            .get_cooldown_until(&subject)
            .expect("get")
            .is_none()
    );

    let ov = f
        .remote
        .upsert_daily_override(&subject, day(), Some(true), Some(300))
        .expect("upsert");
    assert_eq!(ov.quota_delta_seconds, Some(300));
    assert_eq!(f.remote.list_daily_overrides(day()).expect("list").len(), 1);
    assert!(
        f.remote
            .clear_daily_override(&subject, day())
            .expect("clear")
    );
    assert!(
        f.remote
            .list_daily_overrides(day())
            .expect("list")
            .is_empty()
    );
}

#[test]
fn audit_settings_and_snapshots_survive_the_wire() {
    let f = fixture();

    f.remote
        .append_audit(AuditEvent::new(AuditEventType::ServiceStarted))
        .expect("audit");
    assert_eq!(f.remote.get_recent_audits(10).expect("audits").len(), 1);

    assert!(f.remote.get_setting("k").expect("get").is_none());
    f.remote.set_setting("k", "v").expect("set");
    assert_eq!(
        f.remote.get_setting("k").expect("get"),
        Some("v".to_string())
    );

    assert!(f.remote.load_snapshot().expect("load").is_none());
    f.remote
        .save_snapshot(&StateSnapshot {
            timestamp: lunchbox_util::now(),
            active_session: None,
        })
        .expect("save");
    assert!(f.remote.load_snapshot().expect("load").is_some());

    assert!(f.remote.is_healthy());
}

/// A store whose every call fails, so the error path can be driven from the
/// server side rather than simulated on the client.
struct FailingStore;

macro_rules! fail {
    () => {
        Err(StoreError::NotFound("nothing here".into()))
    };
}

impl Store for FailingStore {
    fn append_audit(&self, _: AuditEvent) -> lunchbox_store::StoreResult<()> {
        fail!()
    }
    fn get_recent_audits(&self, _: usize) -> lunchbox_store::StoreResult<Vec<AuditEvent>> {
        fail!()
    }
    fn get_usage(&self, _: &EntryId, _: NaiveDate) -> lunchbox_store::StoreResult<Duration> {
        fail!()
    }
    fn add_usage(&self, _: &EntryId, _: NaiveDate, _: Duration) -> lunchbox_store::StoreResult<()> {
        fail!()
    }
    fn get_token_state(
        &self,
        _: &LimitSubject,
        _: NaiveDate,
        _: bool,
    ) -> lunchbox_store::StoreResult<TokenState> {
        fail!()
    }
    fn adjust_token_balance(
        &self,
        _: &LimitSubject,
        _: NaiveDate,
        _: bool,
        _: i64,
    ) -> lunchbox_store::StoreResult<TokenState> {
        fail!()
    }
    fn get_cooldown_until(
        &self,
        _: &LimitSubject,
    ) -> lunchbox_store::StoreResult<Option<chrono::DateTime<Local>>> {
        fail!()
    }
    fn set_cooldown_until(
        &self,
        _: &LimitSubject,
        _: chrono::DateTime<Local>,
    ) -> lunchbox_store::StoreResult<()> {
        fail!()
    }
    fn clear_cooldown(&self, _: &LimitSubject) -> lunchbox_store::StoreResult<()> {
        fail!()
    }
    fn load_snapshot(&self) -> lunchbox_store::StoreResult<Option<StateSnapshot>> {
        fail!()
    }
    fn save_snapshot(&self, _: &StateSnapshot) -> lunchbox_store::StoreResult<()> {
        fail!()
    }
    fn is_healthy(&self) -> bool {
        false
    }
    fn get_daily_override(
        &self,
        _: &LimitSubject,
        _: NaiveDate,
    ) -> lunchbox_store::StoreResult<Option<lunchbox_api::DailyOverride>> {
        fail!()
    }
    fn upsert_daily_override(
        &self,
        _: &LimitSubject,
        _: NaiveDate,
        _: Option<bool>,
        _: Option<i64>,
    ) -> lunchbox_store::StoreResult<lunchbox_api::DailyOverride> {
        fail!()
    }
    fn clear_daily_override(
        &self,
        _: &LimitSubject,
        _: NaiveDate,
    ) -> lunchbox_store::StoreResult<bool> {
        fail!()
    }
    fn list_daily_overrides(
        &self,
        _: NaiveDate,
    ) -> lunchbox_store::StoreResult<Vec<lunchbox_api::DailyOverride>> {
        fail!()
    }
    fn record_audio_output_seen(
        &self,
        _: &lunchbox_api::AudioOutput,
    ) -> lunchbox_store::StoreResult<()> {
        fail!()
    }
    fn set_audio_output_limits(
        &self,
        _: &str,
        _: Option<u8>,
        _: Option<u8>,
    ) -> lunchbox_store::StoreResult<bool> {
        fail!()
    }
    fn get_audio_output(
        &self,
        _: &str,
    ) -> lunchbox_store::StoreResult<Option<lunchbox_api::AudioOutputRecord>> {
        fail!()
    }
    fn list_audio_outputs(
        &self,
    ) -> lunchbox_store::StoreResult<Vec<lunchbox_api::AudioOutputRecord>> {
        fail!()
    }
    fn forget_audio_output(&self, _: &str) -> lunchbox_store::StoreResult<bool> {
        fail!()
    }
    fn get_setting(&self, _: &str) -> lunchbox_store::StoreResult<Option<String>> {
        fail!()
    }
    fn set_setting(&self, _: &str, _: &str) -> lunchbox_store::StoreResult<()> {
        fail!()
    }
    fn get_usage_range(
        &self,
        _: &EntryId,
        _: NaiveDate,
        _: NaiveDate,
    ) -> lunchbox_store::StoreResult<Vec<(NaiveDate, Duration)>> {
        fail!()
    }
    fn get_all_usage_for_date(
        &self,
        _: NaiveDate,
    ) -> lunchbox_store::StoreResult<Vec<(EntryId, Duration)>> {
        fail!()
    }
}

#[test]
fn a_store_error_crosses_the_wire_as_the_same_kind() {
    // `StoreError::Io` cannot serialise and `NotFound` means something specific
    // to callers, so the kind travels separately and is rebuilt. Driven from a
    // failing store on the *server* side, because the interesting path is the
    // one where the custodian is what went wrong.
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("state.sock");
    spawn_server(
        socket.clone(),
        Arc::new(FailingStore),
        Arc::new(lunchbox_util::LocalProtectedFiles::new(
            dir.path().to_path_buf(),
        )),
    );
    let remote = RemoteStore::connect_at(socket, nix::unistd::getuid().as_raw()).expect("connect");

    let err = remote
        .get_usage(&EntryId::new("e"), day())
        .expect_err("the store failed, so the call must");
    assert!(
        matches!(err, StoreError::NotFound(ref m) if m.contains("nothing here")),
        "the kind and the message both have to survive: {err:?}"
    );

    // `is_healthy` cannot report why, so a failing store has to read as
    // unhealthy rather than panicking or defaulting to true.
    assert!(!remote.is_healthy());
}

#[test]
fn every_remaining_method_is_wired_to_the_right_store_call() {
    // A dispatch arm wired to the wrong method — `GetSetting` calling
    // `set_setting`, say — would typecheck. The ones not covered by the tests
    // above get a round trip here so a mis-wiring shows up as a wrong answer.
    let f = fixture();
    let entry = EntryId::new("scummvm");

    f.remote
        .add_usage(&entry, day(), Duration::from_secs(45))
        .expect("add");
    let range = f
        .remote
        .get_usage_range(&entry, day(), day())
        .expect("range");
    assert_eq!(range, vec![(day(), Duration::from_secs(45))]);
    let all = f.remote.get_all_usage_for_date(day()).expect("all");
    assert_eq!(all, vec![(entry.clone(), Duration::from_secs(45))]);

    let subject = LimitSubject::entry("scummvm");
    assert!(
        f.remote
            .get_daily_override(&subject, day())
            .expect("none yet")
            .is_none()
    );
    f.remote
        .upsert_daily_override(&subject, day(), Some(false), None)
        .expect("upsert");
    let one = f
        .remote
        .get_daily_override(&subject, day())
        .expect("get")
        .expect("some");
    assert_eq!(one.availability, Some(false));

    let output = lunchbox_api::AudioOutput {
        key: "alsa:hdmi".into(),
        description: "HDMI".into(),
        ..Default::default()
    };
    f.remote.record_audio_output_seen(&output).expect("seen");
    assert_eq!(f.remote.list_audio_outputs().expect("list").len(), 1);
    assert!(
        f.remote
            .set_audio_output_limits("alsa:hdmi", Some(60), Some(10))
            .expect("limits")
    );
    let rec = f
        .remote
        .get_audio_output("alsa:hdmi")
        .expect("get")
        .expect("some");
    assert_eq!(rec.max_volume, Some(60));
    assert_eq!(rec.min_volume, Some(10));
    assert!(f.remote.forget_audio_output("alsa:hdmi").expect("forget"));
    assert!(f.remote.list_audio_outputs().expect("list").is_empty());
}

#[test]
fn the_client_reconnects_when_the_custodian_restarts() {
    // Socket activation means the daemon can come and go under a live client:
    // it exits when the trusted session ends, and the next connection starts it
    // again. A client that could not reconnect would turn that into a device
    // that needs a reboot.
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("lunchboxd.db");
    let socket = dir.path().join("state.sock");
    let direct: Arc<dyn Store> = Arc::new(SqliteStore::open(&db).expect("open"));

    let files: Arc<dyn lunchbox_util::ProtectedFiles> = Arc::new(
        lunchbox_util::LocalProtectedFiles::new(dir.path().to_path_buf()),
    );
    spawn_server(socket.clone(), Arc::clone(&direct), Arc::clone(&files));
    let remote =
        RemoteStore::connect_at(socket.clone(), nix::unistd::getuid().as_raw()).expect("connect");
    let entry = EntryId::new("e");
    remote
        .add_usage(&entry, day(), Duration::from_secs(10))
        .expect("first write");

    // Take the listener away and put a fresh one back, which is what a restart
    // looks like from here.
    std::fs::remove_file(&socket).expect("unlink");
    spawn_server(socket.clone(), Arc::clone(&direct), files);

    // The first call after the break may fail (the old connection is only
    // discovered to be dead when it is used), but the client must recover
    // rather than stay broken.
    let mut recovered = false;
    for _ in 0..3 {
        if remote.get_usage(&entry, day()).is_ok() {
            recovered = true;
            break;
        }
    }
    assert!(recovered, "the client never reconnected after a restart");
    assert_eq!(
        remote.get_usage(&entry, day()).expect("read"),
        Duration::from_secs(10),
        "and it is talking to the same state it was before"
    );
}

#[test]
fn the_protected_files_round_trip_and_take_is_atomic() {
    use lunchbox_state_proto::RemoteFiles;
    use lunchbox_util::{ProtectedFile, ProtectedFiles};

    let f = fixture();
    let files =
        RemoteFiles::connect_at(f.socket.clone(), nix::unistd::getuid().as_raw()).expect("connect");

    // A policy that was never written reads as absent rather than as an error:
    // lunchboxd falls back to its own copy on `None`, and an error there would
    // stop a device booting instead.
    assert!(files.read(ProtectedFile::Config).expect("read").is_none());

    files
        .write(ProtectedFile::Config, "config_version = 1\n")
        .expect("write");
    assert_eq!(
        files.read(ProtectedFile::Config).expect("read"),
        Some("config_version = 1\n".to_string())
    );
    // And it really is a file in the custodian's directory, not something the
    // client remembered.
    assert_eq!(
        std::fs::read_to_string(f.dir.path().join("config.toml")).expect("on disk"),
        "config_version = 1\n"
    );

    // Each name is its own file: a bug that collapsed the enum to one path
    // would have the admin record overwrite the policy.
    files
        .write(ProtectedFile::AdminRecord, "token = \"abc\"\n")
        .expect("write");
    assert_eq!(
        files.read(ProtectedFile::Config).expect("read"),
        Some("config_version = 1\n".to_string()),
        "writing one protected file must not touch another"
    );

    // `take` is why the trait has a third verb. The sentinel is an instruction,
    // and acting on it twice factory-resets a device that was already reset.
    files
        .write(ProtectedFile::ResetSentinel, "")
        .expect("write");
    assert_eq!(
        files.take(ProtectedFile::ResetSentinel).expect("take"),
        Some(String::new())
    );
    assert!(
        files
            .take(ProtectedFile::ResetSentinel)
            .expect("take again")
            .is_none(),
        "a consumed sentinel must not come back"
    );

    assert!(files.delete(ProtectedFile::Config).expect("delete"));
    assert!(
        !files.delete(ProtectedFile::Config).expect("delete again"),
        "deleting what is not there is false, not an error"
    );
}

/// The supervision channel, both halves, over a real socket (issue #172).
///
/// The custodian's connection loop lives in a binary crate and cannot be called
/// from here, so what this pins is the *wire*: that `Supervise` is answered
/// once and then never again, that heartbeats arrive as heartbeats, and — the
/// part the whole watchdog rests on — that a dropped client is an EOF the
/// server can see. A `beat()` that started answering, or a `Supervise` the
/// server had to reply to twice, would deadlock a device rather than fail a
/// test, so it is worth holding both ends together here.
#[test]
fn the_supervision_channel_is_one_reply_and_then_only_beats() {
    use lunchbox_state_proto::{StateRequest, SuperviseReply, Transport};

    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("state.sock");
    let listener = UnixListener::bind(&socket).expect("bind");

    let (tx, rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        let mut writer = stream.try_clone().expect("clone");
        let mut lines = BufReader::new(stream).lines();

        // The handshake `Transport::connect_at` makes before anything else.
        let hello = lines.next().expect("a line").expect("read");
        assert!(matches!(
            serde_json::from_str::<StateRequest>(&hello),
            Ok(StateRequest::Hello { .. })
        ));
        writeln!(
            writer,
            "{}",
            serde_json::to_string(&lunchbox_state_proto::WireResult::Ok {
                value: lunchbox_state_proto::HelloReply {
                    proto: lunchbox_state_proto::PROTO_VERSION,
                },
            })
            .expect("encode")
        )
        .expect("write");

        let request = lines.next().expect("a line").expect("read");
        assert!(matches!(
            serde_json::from_str::<StateRequest>(&request),
            Ok(StateRequest::Supervise)
        ));
        writeln!(
            writer,
            "{}",
            serde_json::to_string(&SuperviseReply {
                armed: true,
                reason: None,
                deadline: Duration::from_secs(30),
            })
            .expect("encode")
        )
        .expect("write");

        // Everything after the reply is one-way.
        let mut beats = 0;
        for line in lines {
            match serde_json::from_str::<StateRequest>(&line.expect("read")) {
                Ok(StateRequest::Heartbeat) => beats += 1,
                other => panic!("only heartbeats belong here, got {other:?}"),
            }
        }
        // Falling out of the loop is the EOF the watchdog fires on.
        tx.send(beats).expect("report");
    });

    let transport = Transport::connect_at(socket, nix::unistd::getuid().as_raw()).expect("connect");
    let (reply, mut supervision) = transport.into_supervision_stream().expect("supervise");
    assert!(reply.armed, "the custodian said it can end the session");
    assert_eq!(reply.deadline, Duration::from_secs(30));

    for _ in 0..3 {
        supervision.beat().expect("beat");
    }
    // Dropping the client is what a killed lunchboxd does to its descriptors,
    // and it is the whole mechanism: the server sees the stream end.
    drop(supervision);

    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5))
            .expect("the server saw the connection end"),
        3
    );
    server.join().expect("server");
}

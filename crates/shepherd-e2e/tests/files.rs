//! The file manager against a running daemon (issue #195).
//!
//! `crates/shepherd-http/tests/` covers the routes in process, which is where
//! the protocol belongs. Three things only a real `shepherdd` can show:
//!
//! - that the routes are **behind the authentication layer**. In process they
//!   are mounted on a router built `without_credential_store`, so every one of
//!   those tests would pass just as happily if the file manager were open to
//!   anyone who could reach the port.
//! - that `enabled` reaches the router at all. It is read from a config file,
//!   by a different crate, at startup; a field that never arrived would be
//!   invisible to a test that constructs the state itself.
//! - that the **denied directories are the daemon's own**. `FileService` is
//!   told a home and works the rest out from the environment, so "the database
//!   this process is actually using is not downloadable" is a sentence only a
//!   running process can be asked.
//!
//! Run alongside the other e2e tests with
//! `cargo test -p shepherd-e2e -- --include-ignored --test-threads=1`.

use anyhow::{Context, Result};
use shepherd_e2e::{HttpClient, TestHarness, json_body};

/// No removable media: the machine running the tests may well have something
/// mounted under `/media` — this repo's own `setup-removable-dev.sh` puts two
/// there — and a roots list that included it would pass or fail by accident.
/// No floor either, so the test does not depend on how full this disk is.
const FILES_CONFIG: &str = r#"
config_version = 1

[service]
default_max_run_seconds = 3600

[service.management_api]
enabled = true
port = {HTTP_PORT}
bind = "127.0.0.1"
{AUTH_TOKEN_LINE}

[service.file_manager]
enabled = {ENABLED}
external_media = false
free_space_floor_bytes = 0

[[entries]]
id = "sleeper"
label = "Sleeper"
[entries.kind]
type = "process"
command = "/usr/bin/sleep"
args = ["600"]
[entries.availability]
always = true
"#;

/// A home directory for the daemon to offer, laid out the way a device's is.
///
/// Its own temp directory rather than the real `$HOME` the harness otherwise
/// passes through: these tests write files, delete them, and ask for a
/// recursive delete, and none of that belongs in the home of whoever is
/// running the suite.
fn fake_home() -> Result<tempfile::TempDir> {
    let home = tempfile::Builder::new()
        .prefix("shepherd-e2e-home-")
        .tempdir()?;
    std::fs::create_dir_all(home.path().join("Books"))?;
    std::fs::write(
        home.path().join("Books/hobbit.epub"),
        b"in a hole in the ground",
    )?;
    // Where this daemon's `XDG_DATA_HOME` says its state lives, which is the
    // layout on a device. (The harness starts shepherdd with an explicit
    // `-d <temp dir>`, so the live database is elsewhere; that path is refused
    // too, and by a route the harness cannot reach through `root=home`. See
    // `a_store_somewhere_else_is_refused_by_the_path_it_really_has` in
    // `crates/shepherd-http/tests/files.rs`.)
    std::fs::create_dir_all(home.path().join(".local/share/shepherdd"))?;
    std::fs::write(
        home.path().join(".local/share/shepherdd/shepherdd.db"),
        b"sqlite",
    )?;
    // A private key, which is the one thing here that is a credential for
    // somewhere else.
    std::fs::create_dir_all(home.path().join(".ssh"))?;
    std::fs::write(home.path().join(".ssh/id_ed25519"), b"-----BEGIN-----\n")?;
    Ok(home)
}

async fn harness(home: &std::path::Path, enabled: bool) -> Result<TestHarness> {
    TestHarness::builder()
        .config_toml(FILES_CONFIG.replace("{ENABLED}", if enabled { "true" } else { "false" }))
        .shepherdd_env("HOME", home.to_string_lossy().to_string())
        // So the daemon's database really is under the home it is offering.
        .shepherdd_env(
            "XDG_DATA_HOME",
            home.join(".local/share").to_string_lossy().to_string(),
        )
        .start()
        .await
}

/// Percent-encode a value for a query string.
fn q(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn content(root: &str, path: &str) -> String {
    format!("/api/v1/files/content?root={}&path={}", q(root), q(path))
}

/// Deterministic bytes, big enough that nothing is buffering the whole thing
/// by accident, and not all one value so an off-by-a-chunk shows as a
/// mismatch. 251 is prime, so it lines up with no power-of-two buffer.
fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn a_file_goes_to_a_real_device_and_comes_back() -> Result<()> {
    let home = fake_home()?;
    let h = harness(home.path(), true).await?;
    let http = h.http();

    // The home the *daemon* resolved, not one this test handed to a
    // constructor.
    let roots = json_body(&http.get("/api/v1/files/roots").await?)?;
    let listed = roots["roots"].as_array().context("no roots")?;
    assert_eq!(listed.len(), 1, "roots: {roots}");
    assert_eq!(listed[0]["id"], "home");
    assert_eq!(listed[0]["writable"], true);
    assert_eq!(
        std::path::Path::new(listed[0]["path"].as_str().unwrap()),
        home.path().canonicalize()?,
    );

    let bytes = payload(3 * 1024 * 1024);
    let upload = http
        .send(
            "PUT",
            &content("home", "Books/film.bin"),
            &[
                ("Content-Type", "application/octet-stream"),
                ("If-None-Match", "*"),
            ],
            &bytes,
        )
        .await?;
    assert_eq!(upload.status, 201, "{}", upload.body);
    let etag = upload
        .header("etag")
        .context("no ETag on the upload")?
        .to_string();
    assert_eq!(
        std::fs::read(home.path().join("Books/film.bin"))?,
        bytes,
        "what landed on disk is not what was sent"
    );

    // Back out through the socket, byte for byte.
    let down = http.get(&content("home", "Books/film.bin")).await?;
    assert_eq!(down.status, 200);
    assert_eq!(down.bytes, bytes, "the download came back changed");
    // The headers that keep an uploaded `.html` from running as this origin.
    assert!(
        down.header("content-disposition")
            .is_some_and(|v| v.starts_with("attachment")),
        "{:?}",
        down.header("content-disposition")
    );
    assert_eq!(down.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(down.header("accept-ranges"), Some("bytes"));

    // And a resumed download, which is a real socket doing a real seek.
    let start = bytes.len() - 900_000;
    let ranged = http
        .send(
            "GET",
            &content("home", "Books/film.bin"),
            &[("Range", &format!("bytes={start}-")), ("If-Range", &etag)],
            &[],
        )
        .await?;
    assert_eq!(ranged.status, 206, "{}", ranged.body);
    assert_eq!(
        ranged.header("content-range"),
        Some(format!("bytes {start}-{}/{}", bytes.len() - 1, bytes.len()).as_str())
    );
    assert_eq!(ranged.bytes, bytes[start..]);

    // Rename, then delete with the tag the upload handed back.
    let moved = http
        .post_json(
            "/api/v1/files/move",
            &serde_json::json!({ "root": "home", "from": "Books/film.bin", "to": "Books/tape.bin" }),
        )
        .await?;
    assert_eq!(moved.status, 204, "{}", moved.body);
    assert!(home.path().join("Books/tape.bin").exists());

    let deleted = http
        .send(
            "DELETE",
            "/api/v1/files/entry?root=home&path=Books%2Ftape.bin",
            &[("If-Match", "*")],
            &[],
        )
        .await?;
    assert_eq!(deleted.status, 204, "{}", deleted.body);
    assert!(!home.path().join("Books/tape.bin").exists());

    h.shutdown().await
}

/// A file whose name is not text, renamed and removed through a real socket.
///
/// Worth doing end to end rather than only through the router: the whole
/// reason a handle exists is that a query string is decoded to a `String`
/// before any handler sees it, so the one thing that must be proved is that
/// this spelling survives a real HTTP parser where the name itself cannot.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn a_name_that_is_not_text_can_be_repaired_or_removed_through_a_real_socket() -> Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let home = fake_home()?;
    // `café.mp3` as a FAT drive mounted with the kernel's default charset
    // hands it back.
    let raw = b"caf\xe9.mp3";
    std::fs::write(
        home.path()
            .join("Books")
            .join(std::ffi::OsStr::from_bytes(raw)),
        b"a song",
    )?;
    let h = harness(home.path(), true).await?;
    let http = h.http();

    let listing = json_body(&http.get("/api/v1/files/list?root=home&path=Books").await?)?;
    let entry = listing["entries"]
        .as_array()
        .context("no entries")?
        .iter()
        .find(|e| e["unusable"] == "name_not_utf8")
        .context("the un-typable file was not listed")?;
    let handle = entry["handle"].as_str().context("no handle")?.to_string();
    assert_eq!(handle, "636166e92e6d7033");

    // The repair first: give it a name that can be typed, and check the bytes
    // are the same file rather than a new empty one.
    let renamed = http
        .post_json(
            "/api/v1/files/move",
            &serde_json::json!({
                "root": "home",
                "from": "Books",
                "to": "Books/cafe.mp3",
                "from_handle": handle,
            }),
        )
        .await?;
    assert_eq!(renamed.status, 204, "{}", renamed.body);
    assert_eq!(
        std::fs::read_to_string(home.path().join("Books/cafe.mp3"))?,
        "a song"
    );
    assert!(
        !home
            .path()
            .join("Books")
            .join(std::ffi::OsStr::from_bytes(raw))
            .exists()
    );

    // Put it back under the un-typable name, and take the other route.
    std::fs::rename(
        home.path().join("Books/cafe.mp3"),
        home.path()
            .join("Books")
            .join(std::ffi::OsStr::from_bytes(raw)),
    )?;
    let listing = json_body(&http.get("/api/v1/files/list?root=home&path=Books").await?)?;
    let entry = listing["entries"]
        .as_array()
        .context("no entries")?
        .iter()
        .find(|e| e["unusable"] == "name_not_utf8")
        .context("not listed the second time")?;

    let deleted = http
        .send(
            "DELETE",
            &format!("/api/v1/files/entry?root=home&path=Books&handle={handle}"),
            &[("If-Match", entry["etag"].as_str().context("no etag")?)],
            &[],
        )
        .await?;
    assert_eq!(deleted.status, 204, "{}", deleted.body);
    assert!(
        !home
            .path()
            .join("Books")
            .join(std::ffi::OsStr::from_bytes(raw))
            .exists(),
        "the file is still there"
    );
    assert!(
        home.path().join("Books/hobbit.epub").exists(),
        "its neighbour went too"
    );

    h.shutdown().await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn the_file_routes_are_behind_the_same_door_as_everything_else() -> Result<()> {
    let home = fake_home()?;
    let h = harness(home.path(), true).await?;

    // In process these routes are mounted on a router built without a
    // credential store, so nothing there can tell whether they are protected.
    // A device on a home network with an unauthenticated file manager is the
    // worst outcome this feature has.
    let anonymous = HttpClient::new(h.http_port(), None);
    for path in [
        "/api/v1/files/roots",
        "/api/v1/files/list?root=home&path=",
        "/api/v1/files/content?root=home&path=Books%2Fhobbit.epub",
    ] {
        let res = anonymous.get(path).await?;
        assert_eq!(
            res.status, 401,
            "{path} answered {}: {}",
            res.status, res.body
        );
    }
    // Writes too, and by their own method rather than by inheriting a guard
    // that only covers `GET`.
    let res = anonymous
        .send(
            "PUT",
            &content("home", "Books/planted.txt"),
            &[("If-None-Match", "*")],
            b"x",
        )
        .await?;
    assert_eq!(res.status, 401, "{}", res.body);
    assert!(!home.path().join("Books/planted.txt").exists());

    h.shutdown().await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn the_daemon_will_not_hand_over_its_own_state() -> Result<()> {
    let home = fake_home()?;
    let h = harness(home.path(), true).await?;
    let http = h.http();

    // Worked out by the daemon from its own environment rather than handed to
    // a constructor by this test — which is the only thing that makes the
    // refusal a statement about the running process.
    let listed = http
        .get("/api/v1/files/list?root=home&path=.local%2Fshare%2Fshepherdd")
        .await?;
    assert_eq!(listed.status, 403, "{}", listed.body);

    let key = http
        .get("/api/v1/files/content?root=home&path=.ssh%2Fid_ed25519")
        .await?;
    assert_eq!(key.status, 403, "{}", key.body);

    // Both are still *visible*, so a person can see what is there and why they
    // cannot have it — and can still delete a stray key if they want to.
    let top = json_body(&http.get("/api/v1/files/list?root=home&path=").await?)?;
    let ssh = top["entries"]
        .as_array()
        .context("no entries")?
        .iter()
        .find(|e| e["name"] == ".ssh")
        .context(".ssh was hidden from the listing entirely")?;
    assert_eq!(ssh["unusable"], "not_browsable");

    // And the log is deliberately *not* refused: pulling shepherdd.log off a
    // device with no shell is one of the better things this feature buys.
    let state = http
        .get("/api/v1/files/list?root=home&path=.local%2Fstate")
        .await?;
    assert!(
        state.status == 200 || state.status == 404,
        "the state directory was refused outright: {} {}",
        state.status,
        state.body
    );

    h.shutdown().await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn an_upload_survives_the_connection_it_started_on() -> Result<()> {
    let home = fake_home()?;
    let h = harness(home.path(), true).await?;
    let http = h.http();

    // Every request here is its own TCP connection, which is the point: a
    // resumable upload has to survive the socket it began on, because on the
    // wifi chip of a repurposed laptop it will not get to keep one.
    let bytes = payload(2 * 1024 * 1024);
    let half = bytes.len() / 2;
    let token = "u-e2e00001";
    let path = content("home", "Books/film.bin");
    let uri = format!("{path}&upload={token}");

    let first = http
        .send(
            "PUT",
            &uri,
            &[
                ("If-None-Match", "*"),
                (
                    "Content-Range",
                    &format!("bytes 0-{}/{}", half - 1, bytes.len()),
                ),
            ],
            &bytes[..half],
        )
        .await?;
    assert_eq!(first.status, 204, "{}", first.body);
    assert_eq!(
        first.header("upload-offset"),
        Some(half.to_string().as_str())
    );

    // What a client does after a drop: ask where the device got to rather than
    // assume.
    let offset = json_body(
        &http
            .get(&format!(
                "/api/v1/files/upload?root=home&path={}&upload={token}",
                q("Books/film.bin")
            ))
            .await?,
    )?["offset"]
        .as_u64()
        .context("no offset")? as usize;
    assert_eq!(offset, half);

    // The half-written file is not visible under its real name yet.
    assert!(!home.path().join("Books/film.bin").exists());

    let second = http
        .send(
            "PUT",
            &uri,
            &[
                ("If-None-Match", "*"),
                (
                    "Content-Range",
                    &format!("bytes {offset}-{}/{}", bytes.len() - 1, bytes.len()),
                ),
            ],
            &bytes[offset..],
        )
        .await?;
    assert_eq!(second.status, 201, "{}", second.body);
    assert_eq!(std::fs::read(home.path().join("Books/film.bin"))?, bytes);

    h.shutdown().await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn a_device_that_does_not_want_the_surface_does_not_have_one() -> Result<()> {
    let home = fake_home()?;
    let h = harness(home.path(), false).await?;
    let http = h.http();

    // `enabled = false` is read from a file, by a different crate, at startup.
    // Not a 403: the routes are not mounted at all, so there is nothing there
    // to refuse people from.
    for path in [
        "/api/v1/files/roots",
        "/api/v1/files/list?root=home&path=",
        "/api/v1/files/content?root=home&path=Books%2Fhobbit.epub",
    ] {
        let res = http.get(path).await?;
        assert_eq!(
            res.status, 404,
            "{path} answered {}: {}",
            res.status, res.body
        );
    }

    // And the rest of the API is unaffected — switching the file manager off
    // is not switching the device off.
    let health = http.rpc("health", serde_json::Value::Null).await?;
    assert_eq!(health.status, 200, "{}", health.body);

    h.shutdown().await
}

//! End-to-end test for the RetroArch core and content diagnostics (#129).
//!
//! The rules and the probes are unit-tested where they live; what only a
//! running daemon can show is that the sweep actually asks the questions — that
//! the core and the content named in the config are checked against the disk at
//! startup and reach a client as per-activity diagnostics. A wiring bug there
//! (a fact never gathered, an entry never visited) is invisible to both unit
//! tests.
//!
//! The core directory is pointed at a scratch dir, and the content paths are
//! written into one, so the result does not depend on what the machine running
//! the test happens to have installed.
//!
//! Run alongside the other e2e tests with
//! `cargo test -p lunchbox-e2e -- --include-ignored --test-threads=1`.

use anyhow::Result;
use serde_json::json;
use lunchbox_e2e::{TestHarness, json_body};

const RETROARCH_CONFIG: &str = r#"
config_version = 1

[service]
default_max_run_seconds = 3600

[service.management_api]
enabled = true
port = {HTTP_PORT}
bind = "127.0.0.1"
{AUTH_TOKEN_LINE}

[[entries]]
id = "healthy"
label = "Healthy"
[entries.kind]
type = "retroarch"
core = "mgba"
content = "{ROM}"
[entries.availability]
always = true

[[entries]]
id = "missing-core"
label = "Missing Core"
[entries.kind]
type = "retroarch"
core = "nosuchcore"
content = "{ROM}"
[entries.availability]
always = true

[[entries]]
id = "missing-core-path"
label = "Missing Core Path"
[entries.kind]
type = "retroarch"
core_path = "/nonexistent/nope_libretro.so"
content = "{ROM}"
[entries.availability]
always = true

[[entries]]
id = "missing-content"
label = "Missing Content"
[entries.kind]
type = "retroarch"
core = "mgba"
content = "/nonexistent/nosuchgame.gba"
[entries.availability]
always = true
"#;

/// What is absent is reported against its own activity, and what is present is
/// not reported at all — the second half matters as much as the first, since a
/// probe that flagged everything would be ignored.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn missing_cores_and_content_reach_clients_as_per_entry_diagnostics() -> Result<()> {
    let scratch = tempfile::tempdir()?;
    std::fs::write(scratch.path().join("mgba_libretro.so"), b"")?;
    let rom = scratch.path().join("game.gba");
    std::fs::write(&rom, b"")?;

    let h = TestHarness::builder()
        .config_toml(RETROARCH_CONFIG.replace("{ROM}", &rom.to_string_lossy()))
        .shepherdd_env(
            "SHEPHERD_LIBRETRO_DIR",
            scratch.path().to_string_lossy().to_string(),
        )
        .start()
        .await?;
    let http = h.http();

    // The sweep runs at startup, but not necessarily before the harness's
    // first request lands.
    let mut flagged: Vec<(String, String)> = Vec::new();
    for _ in 0..40 {
        let set = json_body(&http.rpc("list_diagnostics", json!({})).await?)?;
        flagged = set["items"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|d| {
                let code = d["code"].as_str()?;
                if !code.starts_with("retroarch_") {
                    return None;
                }
                Some((
                    code.to_string(),
                    d["subject"]["entry_id"].as_str()?.to_string(),
                ))
            })
            .collect();
        if flagged.len() == 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    flagged.sort();
    assert_eq!(
        flagged,
        vec![
            (
                "retroarch_content_missing".to_string(),
                "missing-content".to_string()
            ),
            (
                "retroarch_core_missing".to_string(),
                "missing-core".to_string()
            ),
            (
                "retroarch_core_missing".to_string(),
                "missing-core-path".to_string()
            ),
        ],
        "expected exactly the entries whose core or content is absent to be flagged"
    );

    h.shutdown().await?;
    Ok(())
}

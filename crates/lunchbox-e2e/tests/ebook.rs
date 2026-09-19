//! End-to-end test for the `ebook` kind (#160).
//!
//! Two things only a running daemon can show. First, that the book-missing
//! probe is actually wired into the sweep — a fact never gathered or an entry
//! never visited is invisible to a unit test of the rules. Second, and more
//! interesting, that launching a reading activity really does materialize the
//! reader's configuration: the restrictions that keep a child in the book are
//! generated at spawn, so a spawn path that skipped them would leave the
//! activity wide open and every unit test still green.
//!
//! The reader binary is stubbed, so the test does not need Okular installed and
//! does not open a window.
//!
//! Run alongside the other e2e tests with
//! `cargo test -p lunchbox-e2e -- --include-ignored --test-threads=1`.

use anyhow::Result;
use lunchbox_e2e::{TestHarness, json_body};
use serde_json::json;

const EBOOK_CONFIG: &str = r#"
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
label = "A Book"
[entries.kind]
type = "ebook"
book = "{BOOK}"
command = "{READER}"
font_size = 18
layout = "single"
[entries.availability]
always = true

[[entries]]
id = "missing-book"
label = "Gone"
[entries.kind]
type = "ebook"
book = "/nonexistent/nosuchbook.epub"
command = "{READER}"
[entries.availability]
always = true

[[entries]]
id = "missing-reader"
label = "No Reader"
[entries.kind]
type = "ebook"
book = "{BOOK}"
command = "/nonexistent/no-such-reader"
[entries.availability]
always = true
"#;

/// A stub that stands in for the reader: it maps no window and waits to be
/// stopped, which is all the spawn path needs of it.
fn stub_reader(dir: &std::path::Path) -> Result<std::path::PathBuf> {
    let path = dir.join("stub-reader");
    std::fs::write(&path, "#!/bin/sh\nexec tail -f /dev/null\n")?;
    let mut perms = std::fs::metadata(&path)?.permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&path, perms)?;
    Ok(path)
}

/// A stand-in for Okular's EPUB backend, which ships separately from Okular on
/// Ubuntu and so is absent from every CI runner.
///
/// Without it the format probe correctly reports the backend missing for every
/// `.epub` entry, and a test about *books* fails on a machine that simply has
/// no reader installed. Returns the directory to hand the daemon as
/// `QT_PLUGIN_PATH`.
fn fake_epub_backend(dir: &std::path::Path) -> Result<std::path::PathBuf> {
    let plugins = dir.join("qtplugins");
    let generators = plugins.join("okular_generators");
    std::fs::create_dir_all(&generators)?;
    std::fs::write(generators.join("okularGenerator_epub.so"), b"")?;
    Ok(plugins)
}

/// A book that is not there is reported against its own activity, and a reader
/// that is not installed against its own; anything healthy is not reported at
/// all — which matters as much as the first two, since a probe that flagged
/// everything would be ignored.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn a_missing_book_reaches_clients_as_a_per_entry_diagnostic() -> Result<()> {
    let scratch = tempfile::tempdir()?;
    let book = scratch.path().join("book.epub");
    std::fs::write(&book, b"")?;
    let reader = stub_reader(scratch.path())?;
    let plugins = fake_epub_backend(scratch.path())?;

    let h = TestHarness::builder()
        .config_toml(
            EBOOK_CONFIG
                .replace("{BOOK}", &book.to_string_lossy())
                .replace("{READER}", &reader.to_string_lossy()),
        )
        .lunchboxd_env("QT_PLUGIN_PATH", plugins.to_string_lossy().to_string())
        .start()
        .await?;
    let http = h.http();

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
                if !code.starts_with("ebook_") {
                    return None;
                }
                Some((
                    code.to_string(),
                    d["subject"]["entry_id"].as_str()?.to_string(),
                ))
            })
            .collect();
        if flagged.len() == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    flagged.sort();
    assert_eq!(
        flagged,
        vec![
            ("ebook_book_missing".to_string(), "missing-book".to_string()),
            (
                "ebook_reader_missing".to_string(),
                "missing-reader".to_string()
            ),
        ],
        "expected exactly the entry whose book is absent, and the one whose reader is"
    );

    h.shutdown().await?;
    Ok(())
}

/// Launching a reading activity generates the reader's configuration.
///
/// The assertions are the two that decide whether the activity is supervised at
/// all: the restrictions are present *and* marked immutable, and the reader is
/// pointed at that configuration rather than the user's own.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn launching_materializes_the_readers_kiosk_configuration() -> Result<()> {
    let scratch = tempfile::tempdir()?;
    let book = scratch.path().join("book.epub");
    std::fs::write(&book, b"")?;
    let reader = stub_reader(scratch.path())?;
    let plugins = fake_epub_backend(scratch.path())?;
    let state_root = scratch.path().join("ebook-state");

    let h = TestHarness::builder()
        .config_toml(
            EBOOK_CONFIG
                .replace("{BOOK}", &book.to_string_lossy())
                .replace("{READER}", &reader.to_string_lossy()),
        )
        .lunchboxd_env(
            "LUNCHBOX_EBOOK_ROOT",
            state_root.to_string_lossy().to_string(),
        )
        .lunchboxd_env("QT_PLUGIN_PATH", plugins.to_string_lossy().to_string())
        .start()
        .await?;
    let http = h.http();

    let launched = json_body(&http.rpc("launch", json!({ "id": "healthy" })).await?)?;
    assert!(
        launched.get("Denied").is_none(),
        "launch was refused: {launched}"
    );

    let config = state_root.join("healthy/config");
    let mut ready = false;
    for _ in 0..40 {
        if config.join("kdeglobals").exists() {
            ready = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(ready, "the reader configuration was never written");

    let kdeglobals = std::fs::read_to_string(config.join("kdeglobals"))?;
    assert!(
        kdeglobals.contains("[KDE Action Restrictions][$i]"),
        "restrictions must be immutable, or the reader writes over them: {kdeglobals}"
    );
    assert!(
        kdeglobals.contains("action/file_open=false"),
        "Ctrl+O is a filesystem browser and must be closed: {kdeglobals}"
    );

    // The per-entry settings reached the generated files, so the entry's own
    // configuration is what the reader will be started with.
    let partrc = std::fs::read_to_string(config.join("okularpartrc"))?;
    assert!(partrc.contains("ViewMode=Single"), "{partrc}");
    assert!(partrc.contains("ViewContinuous=false"), "{partrc}");
    let font = std::fs::read_to_string(config.join("okular_epub_generator_settings"))?;
    assert!(
        font.contains(",18,"),
        "font size did not reach the reader: {font}"
    );

    // Reading positions live under the same root, and nothing else may.
    assert!(state_root.join("healthy/data").is_dir());

    http.rpc("stop_current", json!({})).await?;
    h.shutdown().await?;
    Ok(())
}

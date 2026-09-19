//! Integration tests for `load_library` against the fixture corpus.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use lunchbox_media_core::{LibraryError, load_library};

#[derive(Debug, Deserialize)]
struct Manifest {
    fixtures: Vec<FixtureEntry>,
}

#[derive(Debug, Deserialize)]
struct FixtureEntry {
    file: String,
    expect: String,
    #[serde(default)]
    error_kind: Option<String>,
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn load_manifest() -> Manifest {
    let path = fixtures_dir().join("validity.toml");
    let content = std::fs::read_to_string(&path).expect("read validity.toml");
    toml::from_str(&content).expect("parse validity.toml")
}

#[test]
fn corpus_matches_manifest() {
    let manifest = load_manifest();
    assert!(!manifest.fixtures.is_empty(), "manifest must have fixtures");

    for fixture in &manifest.fixtures {
        let path = fixtures_dir().join(&fixture.file);
        let result = load_library(&path);

        match (fixture.expect.as_str(), &result) {
            ("ok", Ok(_)) => {}
            ("ok", Err(e)) => {
                panic!("fixture `{}` expected ok but got error: {e}", fixture.file)
            }
            ("fail", Ok(_)) => {
                panic!("fixture `{}` expected fail but parsed ok", fixture.file)
            }
            ("fail", Err(e)) => {
                let kind = error_kind_label(e);
                if let Some(expected) = &fixture.error_kind
                    && kind != expected
                {
                    panic!(
                        "fixture `{}` expected error_kind=`{}` but got `{}` ({e})",
                        fixture.file, expected, kind
                    );
                }
            }
            (other, _) => panic!("invalid expect value `{other}` in manifest"),
        }
    }
}

#[test]
fn m3u_playlist_derives_library_id_from_filename() {
    let lib = load_library(&fixtures_dir().join("valid-playlist.m3u")).unwrap();
    assert_eq!(lib.library_id, "valid-playlist");
    assert_eq!(lib.items.len(), 3);
    assert_eq!(lib.items[0].id, "track-001");
}

#[test]
fn m3u8_extinf_populates_titles_and_durations() {
    let lib = load_library(&fixtures_dir().join("valid-playlist-extinf.m3u8")).unwrap();
    assert_eq!(lib.items.len(), 4);
    assert_eq!(lib.items[0].title, "Big Buck Bunny");
    assert_eq!(lib.items[0].duration_seconds, Some(596));
    assert_eq!(lib.items[2].title, "Lofi Beats (live)");
    assert_eq!(lib.items[2].duration_seconds, None);
}

fn error_kind_label(e: &LibraryError) -> &'static str {
    match e {
        LibraryError::Read { .. } => "read",
        LibraryError::Parse { .. } => "parse",
        LibraryError::UnsupportedSchema { .. } => "unsupported-schema",
        LibraryError::InvalidLibraryId { .. } => "invalid-library-id",
        LibraryError::InvalidTitle { .. } => "invalid-title",
        LibraryError::InvalidItemId { .. } => "invalid-item-id",
        LibraryError::InvalidItemTitle { .. } => "invalid-item-title",
        LibraryError::InvalidCategory { .. } => "invalid-category",
        LibraryError::EmptySources { .. } => "empty-sources",
        LibraryError::EmptyPlatforms { .. } => "empty-platforms",
        LibraryError::DuplicateItemId { .. } => "duplicate-id",
        LibraryError::BadUri { .. } => "bad-uri",
        LibraryError::DrmRejected { .. } => "drm-rejected",
        LibraryError::BadPoster { .. } => "bad-poster",
    }
}

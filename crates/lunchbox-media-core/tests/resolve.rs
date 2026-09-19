//! Source resolution against fixture libraries.

use std::path::{Path, PathBuf};

use lunchbox_media_core::{ClassifiedUri, Platform, PlatformInfo, load_library, resolve_source};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn linux() -> PlatformInfo {
    PlatformInfo {
        platform: Platform::Linux,
    }
}

fn android() -> PlatformInfo {
    PlatformInfo {
        platform: Platform::Android,
    }
}

#[test]
fn local_only_resolves_on_linux_for_every_item() {
    let lib = load_library(&fixtures_dir().join("valid-local-only.toml")).unwrap();
    for item in &lib.items {
        let s = resolve_source(item, &linux()).expect("source resolves on linux");
        assert!(
            matches!(s.uri, ClassifiedUri::Local(_)),
            "expected local uri, got {:?}",
            s.uri
        );
    }
}

#[test]
fn youtube_only_resolves_on_any_platform() {
    let lib = load_library(&fixtures_dir().join("valid-youtube-only.toml")).unwrap();
    for item in &lib.items {
        let s = resolve_source(item, &linux()).expect("resolves on linux via *");
        assert!(matches!(s.uri, ClassifiedUri::YouTube(_)));
        let s = resolve_source(item, &android()).expect("resolves on android via *");
        assert!(matches!(s.uri, ClassifiedUri::YouTube(_)));
    }
}

#[test]
fn platform_fallback_picks_first_match() {
    let lib = load_library(&fixtures_dir().join("valid-platform-fallback.toml")).unwrap();

    // local-then-stream: Linux picks the file, Android picks the HTTP stream
    let item = lib
        .items
        .iter()
        .find(|i| i.id == "local-then-stream")
        .unwrap();
    let linux_src = resolve_source(item, &linux()).unwrap();
    assert!(matches!(linux_src.uri, ClassifiedUri::Local(_)));
    let android_src = resolve_source(item, &android()).unwrap();
    assert!(matches!(android_src.uri, ClassifiedUri::DirectHttp(_)));

    // android-only: no source for Linux, source for Android
    let item = lib.items.iter().find(|i| i.id == "android-only").unwrap();
    assert!(resolve_source(item, &linux()).is_none());
    assert!(resolve_source(item, &android()).is_some());

    // fallback-any: Linux picks the local file (first match), even though
    // the second source has `*` which would also match.
    let item = lib.items.iter().find(|i| i.id == "fallback-any").unwrap();
    let s = resolve_source(item, &linux()).unwrap();
    match &s.uri {
        ClassifiedUri::Local(p) => assert!(p.ends_with("preferred.mp4")),
        other => panic!("expected local preferred.mp4, got {other:?}"),
    }
    // Android still gets the `*` fallback because no android-specific source
    // is declared — first-match-wins on `*`.
    let s = resolve_source(item, &android()).unwrap();
    assert!(matches!(s.uri, ClassifiedUri::YouTube(_)));
}

#[test]
fn no_sources_for_any_platform_resolves_to_none_on_linux() {
    let lib =
        load_library(&fixtures_dir().join("invalid-no-sources-for-any-platform.toml")).unwrap();
    let item = &lib.items[0];
    assert!(
        resolve_source(item, &linux()).is_none(),
        "Android-only item must not resolve on Linux"
    );
    assert!(resolve_source(item, &android()).is_some());
}

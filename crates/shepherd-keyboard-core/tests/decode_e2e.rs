//! End-to-end tests that exercise the real signed-bundle decode path through
//! `shepherd-keyboard-core` (Phase 0 gate) and the session state machine over a live
//! decoder (Phase 1 gate: swipe → preedit/suggestions, finalize-on-next-action, and the
//! password gate disabling swipe / decode entirely).
//!
//! These need a real signed bundle, which is never committed. Fetch it first:
//!
//! ```sh
//! scripts/fetch-swipe-bundles.sh
//! ```
//!
//! The bundle root is read from `$SHEPHERD_SWIPE_BUNDLE_DIR` (default
//! `dev-runtime/swipe-bundles`); these tests are `#[ignore]`d so the suite is green without
//! bundles. Run them with `cargo test -p shepherd-keyboard-core -- --include-ignored` once
//! bundles are present (mirrors how the repo gates its environment-dependent E2E tests).

use std::path::PathBuf;

use shepherd_keyboard_core::{
    ContentType, Decoder, FunctionKey, Gesture, HostAction, InputPurpose, Keyboard, Profile,
    Stroke, bundle, decode_gesture_json,
};

const HELLO_GESTURE: &str = include_str!("fixtures/hello.gesture.json");

/// Resolve the bundle root from the environment, falling back to the dev cache the fetch
/// script writes to.
fn bundle_root() -> PathBuf {
    match std::env::var_os("SHEPHERD_SWIPE_BUNDLE_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => {
            // crate dir is .../crates/shepherd-keyboard-core; repo root is two up.
            let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(|p| p.parent())
                .map(PathBuf::from)
                .expect("crate is two levels under the repo root");
            repo_root.join("dev-runtime/swipe-bundles")
        }
    }
}

fn adult_decoder() -> Decoder {
    let dir = bundle::bundle_dir(&bundle_root(), Profile::Adult);
    bundle::load_decoder(&dir, None)
        .unwrap_or_else(|e| panic!("load adult bundle at {}: {e}", dir.display()))
}

/// Parse the committed `hello` fixture into a swipe stroke.
fn hello_swipe() -> Stroke {
    let (gesture, _) = Gesture::from_json(HELLO_GESTURE).expect("fixture parses");
    Stroke::Swipe(gesture)
}

#[test]
#[ignore = "requires a downloaded signed bundle; run scripts/fetch-swipe-bundles.sh first"]
fn hello_gesture_decodes_to_hello_through_the_core() {
    let decoder = adult_decoder();
    let candidates =
        decode_gesture_json(&decoder, HELLO_GESTURE, "").expect("fixture gesture decodes");
    assert!(!candidates.is_empty(), "decode returned no candidates");
    assert_eq!(
        candidates[0].word, "hello",
        "top candidate should be 'hello', got {candidates:?}"
    );
}

#[test]
#[ignore = "requires a downloaded signed bundle; run scripts/fetch-swipe-bundles.sh first"]
fn swipe_sets_preedit_then_space_finalizes_it() {
    let mut kbd = Keyboard::new(adult_decoder());
    assert!(kbd.swipe_enabled());

    let actions = kbd.on_stroke(hello_swipe());
    // Top-1 previewed as preedit; suggestions surfaced.
    assert!(
        actions.contains(&HostAction::SetPreedit("hello".to_string())),
        "expected a 'hello' preedit, got {actions:?}"
    );
    assert_eq!(kbd.preedit(), "hello");
    assert!(kbd.suggestions().contains(&"hello".to_string()));

    // Space finalizes the preedit, then inserts a space.
    let actions = kbd.on_function_key(FunctionKey::Space);
    assert_eq!(
        actions,
        vec![
            HostAction::CommitText("hello".to_string()),
            HostAction::CommitText(" ".to_string()),
            HostAction::SetSuggestions(vec![]),
        ]
    );
    assert_eq!(kbd.preedit(), "");
}

#[test]
#[ignore = "requires a downloaded signed bundle; run scripts/fetch-swipe-bundles.sh first"]
fn second_swipe_finalizes_the_previous_word() {
    let mut kbd = Keyboard::new(adult_decoder());
    kbd.on_stroke(hello_swipe());
    assert_eq!(kbd.preedit(), "hello");
    // A new swipe must first commit the previous preedit.
    let actions = kbd.on_stroke(hello_swipe());
    assert_eq!(
        actions.first(),
        Some(&HostAction::CommitText("hello".to_string())),
        "a new swipe finalizes the previous word first; got {actions:?}"
    );
}

#[test]
#[ignore = "requires a downloaded signed bundle; run scripts/fetch-swipe-bundles.sh first"]
fn selecting_a_suggestion_commits_that_alternate() {
    let mut kbd = Keyboard::new(adult_decoder());
    kbd.on_stroke(hello_swipe());
    let alternates = kbd.suggestions().to_vec();
    assert!(alternates.len() >= 2, "need alternates to pick from");
    let actions = kbd.select_suggestion(1);
    assert_eq!(
        actions.first(),
        Some(&HostAction::CommitText(alternates[1].clone()))
    );
    assert_eq!(kbd.preedit(), "");
}

#[test]
#[ignore = "requires a downloaded signed bundle; run scripts/fetch-swipe-bundles.sh first"]
fn password_field_disables_swipe_and_decode() {
    let mut kbd = Keyboard::new(adult_decoder());
    // Focus a password field.
    kbd.set_content_type(ContentType::new(InputPurpose::Password));
    assert!(
        !kbd.swipe_enabled(),
        "swipe must be off in a password field"
    );

    // A swipe in a password field must produce no actions at all — no decode, no preedit,
    // no suggestions.
    let actions = kbd.on_stroke(hello_swipe());
    assert!(
        actions.is_empty(),
        "swipe in a password field must be a no-op, got {actions:?}"
    );
    assert_eq!(kbd.preedit(), "");
    assert!(kbd.suggestions().is_empty());

    // Tapping still works (plain entry).
    let actions = kbd.on_stroke(Stroke::Tap { x: 0.10047, y: 0.5 });
    assert_eq!(actions, vec![HostAction::CommitText("a".to_string())]);
}

#[test]
#[ignore = "requires a downloaded signed bundle; run scripts/fetch-swipe-bundles.sh first"]
fn entering_password_field_drops_an_active_preedit() {
    let mut kbd = Keyboard::new(adult_decoder());
    kbd.on_stroke(hello_swipe());
    assert_eq!(kbd.preedit(), "hello");
    // Switching to a sensitive field drops the preedit WITHOUT committing it.
    let actions = kbd.set_content_type(ContentType::new(InputPurpose::Password));
    assert!(
        actions.contains(&HostAction::SetPreedit(String::new())),
        "preedit should be cleared (not committed) on entering a password field: {actions:?}"
    );
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, HostAction::CommitText(_))),
        "must not commit the dropped preedit into a password field: {actions:?}"
    );
    assert_eq!(kbd.preedit(), "");
}

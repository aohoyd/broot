//! Integration tests for the `:goto_bookmarks` verb wiring (Task 13).
//!
//! These tests focus on the public surface:
//! - the verb registry registers `Internal::goto_bookmarks` with key `g`,
//! - building the `GotoOverlay` from a list of `BookmarkEntry` produces
//!   the expected `Overlay::Goto` variant carrying the entries,
//! - the overlay's single-character match returns the bookmarked path
//!   (this also doubles as a sanity check for Task 12's behaviour from
//!   the integration layer).
//!
//! The full `App::apply_command` end-to-end pipeline is gapped by the
//! same headless-event-loop limitation noted in `confirm_destructive.rs`
//! and is verified manually per the plan's "Post-Completion" section.

use {
    broot::{
        app::{
            BookmarkEntry,
            GotoOverlay,
            Overlay,
            OverlayOutcome,
            OverlayState,
        },
        conf::Conf,
        verb::{
            Internal,
            VerbStore,
        },
    },
    crokey::{
        key,
        KeyCombination,
    },
    std::path::PathBuf,
};

// =============================================================================
// Verb registry shape
// =============================================================================

/// `Internal::goto_bookmarks` must be registered with key `g` in the
/// built-in verb store.
#[test]
fn goto_bookmarks_internal_is_registered_with_g_key() {
    let mut conf = Conf::default();
    let store = VerbStore::new(&mut conf).unwrap();
    let verb = store
        .verbs()
        .iter()
        .find(|v| v.get_internal() == Some(Internal::goto_bookmarks))
        .expect(":goto_bookmarks must be registered as a built-in internal");
    let g_key: KeyCombination = key!('g');
    assert!(
        verb.keys.contains(&g_key),
        "expected :goto_bookmarks to be bound to 'g', got keys = {:?}",
        verb.keys,
    );
}

/// The `key_desc_of_internal` helper exposed for status lines must
/// resolve `goto_bookmarks` to the `g` keystroke.
#[test]
fn key_desc_of_goto_bookmarks_is_g() {
    let mut conf = Conf::default();
    let store = VerbStore::new(&mut conf).unwrap();
    let desc = store
        .key_desc_of_internal(Internal::goto_bookmarks)
        .expect("goto_bookmarks should have a key description");
    assert_eq!(desc, "g");
}

// =============================================================================
// Overlay construction + dispatch
// =============================================================================

fn entry(
    key: char,
    path: &str,
    label: &str,
) -> BookmarkEntry {
    BookmarkEntry {
        key,
        path: PathBuf::from(path),
        label: label.to_string(),
    }
}

fn sample_entries() -> Vec<BookmarkEntry> {
    vec![
        entry('h', "/home/me", "Home"),
        entry('d', "/home/me/Downloads", "Downloads"),
        entry('c', "/home/me/.config", "config"),
        entry('t', "/home/me/.Trash", "Trash"),
    ]
}

/// Building the overlay from `Internal::goto_bookmarks` yields an
/// `Overlay::Goto` variant; pressing the bookmark key reaches the
/// expected entry. We deliberately do not pin the struct fields here —
/// see `tests/goto_bookmarks.rs::overlay_h_key_returns_close_and_focus_with_home`
/// below for the behavioural surface.
#[test]
fn overlay_goto_constructor_carries_bookmarks() {
    let entries = sample_entries();
    let mut overlay = Overlay::Goto(GotoOverlay::new(entries.clone()));
    // The first entry's key (h) must focus the home path. This pins
    // *behaviour* (carry-through) rather than the struct field shape,
    // letting `GotoOverlay` tighten its visibility without churn here.
    let outcome = overlay.handle_key(key!('h'));
    match outcome {
        OverlayOutcome::CloseAndFocus(p) => assert_eq!(p, entries[0].path),
        other => panic!("expected CloseAndFocus, got {other:?}"),
    }
}

/// Pressing the bookmark key inside the overlay closes it with
/// `CloseAndFocus(<path>)`, the path the App routes through a synthesized
/// `:focus` command (Task 6).
#[test]
fn overlay_h_key_returns_close_and_focus_with_home() {
    let mut overlay = GotoOverlay::new(sample_entries());
    let outcome = overlay.handle_key(key!('h'));
    match outcome {
        OverlayOutcome::CloseAndFocus(path) => {
            assert_eq!(path, PathBuf::from("/home/me"));
        }
        other => panic!("expected CloseAndFocus, got {other:?}"),
    }
}

/// A bookmark whose target path doesn't exist still produces a normal
/// `CloseAndFocus(<path>)` outcome — broot will then surface its own
/// "not found" error when `:focus` runs against the missing path.
#[test]
fn overlay_does_not_pre_validate_paths() {
    let entries = vec![entry('x', "/this/path/definitely/does/not/exist", "ghost")];
    let mut overlay = GotoOverlay::new(entries);
    let outcome = overlay.handle_key(key!('x'));
    match outcome {
        OverlayOutcome::CloseAndFocus(p) => {
            assert_eq!(p, PathBuf::from("/this/path/definitely/does/not/exist"));
        }
        other => panic!("expected CloseAndFocus for non-existent path, got {other:?}"),
    }
}

/// `Esc` dismisses without focusing anything.
#[test]
fn overlay_esc_closes_without_focus() {
    let mut overlay = GotoOverlay::new(sample_entries());
    let outcome = overlay.handle_key(key!(esc));
    assert!(matches!(outcome, OverlayOutcome::Close));
}

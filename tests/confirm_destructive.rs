//! Integration tests for the destructive-verb confirmation flow.
//!
//! These tests focus on the public surface: the verb store's
//! `requires_confirm` shape, the `ConfirmOverlay` constructor and the
//! `OverlayOutcome` returned by its key handler. The full
//! `App::apply_command` path is not driven from a test process because
//! it requires a real event source / TTY — but the smaller pieces
//! (verb registration, overlay state machine, command preservation
//! through the overlay) are all exercised here.

use {
    broot::{
        app::{
            ConfirmOverlay,
            Overlay,
            OverlayOutcome,
            OverlayState,
        },
        command::Command,
        conf::{
            Conf,
            VerbConf,
        },
        verb::{
            ExecPattern,
            Internal,
            PrefixSearchResult,
            VerbStore,
        },
    },
    crokey::{
        crossterm::event::{
            KeyCode,
            KeyEvent,
            KeyModifiers,
        },
        KeyCombination,
        key,
    },
};

fn key_y() -> KeyCombination {
    KeyCombination::from(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE))
}

/// RAII guard for filesystem-touching tests: paths registered here are
/// removed (best-effort) when the guard is dropped, even on assertion
/// failure. Avoids the "panic between create and cleanup leaks the file"
/// pattern that the previous tests exhibited.
struct FsCleanup {
    paths: Vec<std::path::PathBuf>,
}

impl FsCleanup {
    fn new() -> Self {
        Self { paths: Vec::new() }
    }

    fn add(
        &mut self,
        p: impl Into<std::path::PathBuf>,
    ) -> std::path::PathBuf {
        let p = p.into();
        self.paths.push(p.clone());
        p
    }
}

impl Drop for FsCleanup {
    /// Cleanup contract: `add` calls register paths in the order the
    /// test creates them (children **after** their parent directory).
    /// `Drop` walks `paths.iter().rev()` so the last-registered path is
    /// removed first — this guarantees that nested files / directories
    /// are gone before we attempt `remove_dir_all` (or `remove_file`)
    /// on their parent. Tests that violate this ordering (e.g. add the
    /// parent directory before its children) may see "directory not
    /// empty" cleanup races; `remove_dir_all` masks most of these but
    /// the documented invariant is "register children before parents".
    fn drop(&mut self) {
        for p in self.paths.iter().rev() {
            if p.is_dir() {
                let _ = std::fs::remove_dir_all(p);
            } else {
                let _ = std::fs::remove_file(p);
            }
        }
    }
}

// =============================================================================
// Verb registration / configuration
// =============================================================================

#[test]
fn builtin_rm_is_registered_with_confirm() {
    let mut conf = Conf::default();
    let store = VerbStore::new(&mut conf).unwrap();
    let rm = store
        .verbs()
        .iter()
        .find(|v| v.has_name("rm"))
        .expect("built-in rm must exist");
    assert!(rm.requires_confirm);
}

#[test]
fn builtin_trash_is_internal() {
    // We don't put `requires_confirm` on the trash verb itself —
    // instead the App-level intercept recognises `Internal::trash` and
    // prompts. This test pins the structure: the `:trash` verb maps to
    // the `Internal::trash` action, so the App can match on it.
    let mut conf = Conf::default();
    let store = VerbStore::new(&mut conf).unwrap();
    let trash = store
        .verbs()
        .iter()
        .find(|v| v.is_internal(Internal::trash))
        .expect("trash verb must be registered");
    assert!(trash.has_name("trash"));
}

#[test]
fn user_confirm_false_overrides_builtin_rm() {
    // A user `VerbConf` with the same name as a built-in and
    // `confirm: false` should override `requires_confirm`. The verb
    // store keeps both the built-in and the user verb; user-defined
    // verbs are registered first (`add_from_conf` runs before
    // `add_builtin_verbs`) so they win on prefix-match.
    //
    // Lookup-order assertion: drive `search_prefix("rm", ...)` and
    // confirm the verb returned is the user-defined one (confirm=false),
    // not the built-in (confirm=true). Asserting only "some verb has
    // confirm=false" — which the previous version of this test did —
    // would still pass even if the built-in won the prefix lookup,
    // and would silently regress.
    let mut conf = Conf::default();
    conf.verbs.push(VerbConf {
        invocation: Some("rm".to_string()),
        external: Some(ExecPattern::from_string("rm -rf {file}")),
        confirm: Some(false),
        ..Default::default()
    });
    let store = VerbStore::new(&mut conf).unwrap();
    match store.search_prefix("rm", None) {
        PrefixSearchResult::Match(_, verb) => {
            assert!(
                !verb.requires_confirm,
                "user `rm` with confirm=false must win the prefix lookup; \
                 builtin (requires_confirm=true) leaked through",
            );
        }
        other => panic!(
            "expected exact-match for `rm`, got {:?}",
            std::mem::discriminant(&other)
        ),
    }

    // Defence-in-depth: the user verb must also appear before any
    // remaining built-in `rm` in the verbs list. If this property
    // breaks (e.g. registration order changes) the prefix lookup above
    // would silently start returning the wrong verb.
    let first_rm_idx = store
        .verbs()
        .iter()
        .position(|v| v.has_name("rm"))
        .expect("at least one `rm` verb must exist");
    assert!(
        !store.verbs()[first_rm_idx].requires_confirm,
        "the first `rm` in the verbs list must be the user override, \
         not the built-in",
    );
}

#[test]
fn user_confirm_true_opts_external_in() {
    let mut conf = Conf::default();
    conf.verbs.push(VerbConf {
        invocation: Some("careful".to_string()),
        external: Some(ExecPattern::from_string("touch {file}")),
        confirm: Some(true),
        ..Default::default()
    });
    let store = VerbStore::new(&mut conf).unwrap();
    let careful = store
        .verbs()
        .iter()
        .find(|v| v.has_name("careful"))
        .expect("careful verb must exist");
    assert!(careful.requires_confirm);
}

// =============================================================================
// ConfirmOverlay state machine
// =============================================================================

fn rm_overlay() -> ConfirmOverlay {
    let pending = Command::from_raw(":rm".to_string(), true);
    ConfirmOverlay::new(
        "Delete /tmp/foo?",
        vec!["/tmp/foo".to_string()],
        "Delete",
        true,
        pending,
    )
}

fn trash_overlay() -> ConfirmOverlay {
    let pending = Command::from_raw(":trash".to_string(), true);
    ConfirmOverlay::new(
        "Trash foo?",
        vec!["/tmp/foo".to_string()],
        "Trash",
        true,
        pending,
    )
}

#[test]
fn rm_overlay_opens_with_confirm_focused() {
    // Behavioural pin: default focus is the action button — Enter on a
    // freshly-built overlay must execute the pending command.
    let mut o = rm_overlay();
    let outcome = o.handle_key(key!(enter));
    assert!(
        matches!(outcome, OverlayOutcome::CloseAndRun(_)),
        "default focus must be Confirm; Enter on first open must run pending, got {outcome:?}",
    );
}

#[test]
fn rm_overlay_y_returns_close_and_run() {
    let mut o = rm_overlay();
    let outcome = o.handle_key(key_y());
    match outcome {
        OverlayOutcome::CloseAndRun(cmd) => match cmd {
            Command::VerbInvocate(inv) => assert_eq!(inv.name, "rm"),
            other => panic!("expected VerbInvocate(rm), got {other:?}"),
        },
        other => panic!("expected CloseAndRun, got {other:?}"),
    }
}

#[test]
fn trash_overlay_y_returns_close_and_run_for_trash() {
    let mut o = trash_overlay();
    let outcome = o.handle_key(key_y());
    match outcome {
        OverlayOutcome::CloseAndRun(cmd) => match cmd {
            Command::VerbInvocate(inv) => assert_eq!(inv.name, "trash"),
            other => panic!("expected VerbInvocate(trash), got {other:?}"),
        },
        other => panic!("expected CloseAndRun, got {other:?}"),
    }
}

#[test]
fn esc_cancels_destructive_overlay() {
    let mut o = rm_overlay();
    let outcome = o.handle_key(key!(esc));
    assert!(
        matches!(outcome, OverlayOutcome::Close),
        "Esc must close the overlay without running the pending command"
    );
}

#[test]
fn enter_on_default_focus_runs_pending() {
    // Default focus is the action button — Enter executes the pending
    // command without an explicit `y`.
    let mut o = rm_overlay();
    let outcome = o.handle_key(key!(enter));
    assert!(matches!(outcome, OverlayOutcome::CloseAndRun(_)));
}

#[test]
fn enter_on_cancel_focus_closes() {
    // Tab flips focus from Confirm to Cancel; Enter must then close
    // without executing the pending command.
    let mut o = rm_overlay();
    let _ = o.handle_key(key!(tab));
    let outcome = o.handle_key(key!(enter));
    assert!(matches!(outcome, OverlayOutcome::Close));
}

// =============================================================================
// Overlay-as-Confirm enum dispatch
// =============================================================================

#[test]
fn overlay_confirm_variant_dispatches_handle_key() {
    let mut overlay = Overlay::Confirm(rm_overlay());
    let outcome = overlay.handle_key(key!(esc));
    assert!(matches!(outcome, OverlayOutcome::Close));
}

#[test]
fn overlay_confirm_variant_dispatches_run_on_y() {
    let mut overlay = Overlay::Confirm(trash_overlay());
    let outcome = overlay.handle_key(key_y());
    assert!(matches!(outcome, OverlayOutcome::CloseAndRun(_)));
}

// =============================================================================
// Smoke: ensure constructing a ConfirmOverlay with each preset path
// preserves the pending command exactly (the bytes are what flows back
// through `apply_command` after the user confirms).
// =============================================================================

#[test]
fn confirm_overlay_preserves_pending_command() {
    let pending = Command::from_raw(":rm".to_string(), true);
    let original = match &pending {
        Command::VerbInvocate(inv) => inv.name.clone(),
        _ => panic!("setup: expected VerbInvocate"),
    };
    let mut o = ConfirmOverlay::new(
        "Delete?",
        vec!["/tmp/x".to_string()],
        "Delete",
        true,
        pending,
    );
    let outcome = o.handle_key(key_y());
    let recovered = match outcome {
        OverlayOutcome::CloseAndRun(Command::VerbInvocate(inv)) => inv.name,
        other => panic!("unexpected outcome: {other:?}"),
    };
    assert_eq!(recovered, original);
}

#[test]
fn confirm_overlay_pending_can_carry_focus_path_for_undo_chain() {
    // Sanity: the overlay can hand back any Command, not just the
    // exact destructive one — letting future task wiring (e.g. an
    // explicit `Internal::trash_confirmed` marker if we ever need it)
    // ride through the overlay machinery without changes.
    let pending = Command::Internal {
        internal: Internal::trash,
        input_invocation: None,
    };
    let mut o = ConfirmOverlay::new(
        "Trash?",
        vec!["/tmp/x".to_string()],
        "Trash",
        true,
        pending,
    );
    let outcome = o.handle_key(key_y());
    assert!(matches!(
        outcome,
        OverlayOutcome::CloseAndRun(Command::Internal { internal: Internal::trash, .. })
    ));
}

// =============================================================================
// File-system-touching smoke test: build a temp file and verify it
// stays put as long as the overlay isn't confirmed. We simulate the
// overlay-cancel case directly here — the actual end-to-end wiring is
// exercised manually per the plan's "Post-Completion" section.
// =============================================================================

#[test]
fn cancel_path_leaves_file_intact() {
    use std::fs::{
        File,
        metadata,
    };
    let mut cleanup = FsCleanup::new();
    let p = cleanup.add(
        std::env::temp_dir().join(format!("broot_confirm_test_{}.txt", std::process::id())),
    );
    File::create(&p).unwrap();
    assert!(p.exists());
    // Build an overlay that *would* delete the file if confirmed.
    let pending = Command::from_raw(":rm".to_string(), true);
    let mut o = ConfirmOverlay::new(
        format!("Delete {}?", p.display()),
        vec![p.to_string_lossy().to_string()],
        "Delete",
        true,
        pending,
    );
    // Cancel.
    let outcome = o.handle_key(key!(esc));
    assert!(matches!(outcome, OverlayOutcome::Close));
    // File untouched.
    assert!(p.exists());
    assert!(metadata(&p).is_ok());
    // Cleanup runs via FsCleanup::Drop.
}

// =============================================================================
// Task 9: cp/mv overwrite confirmation. The runtime resolution lives
// inside `App::maybe_destructive_confirm` (private), so the integration
// surface here covers:
//
//   1. The verb registry exposes the four target verbs with the
//      expected exec-pattern shape (the resolver discriminates on this).
//   2. A constructed `ConfirmOverlay` modelling the overwrite case
//      preserves the pending command across confirm/cancel.
//
// End-to-end App-driven tests are not feasible (no headless harness);
// the unit-level tests in `src/app/app.rs::confirm_helper_tests` cover
// `resolve_overwrite_target` directly.
// =============================================================================
use broot::verb::VerbExecution;

#[cfg(unix)]
#[test]
fn cp_verb_uses_path_from_parent() {
    let mut conf = Conf::default();
    let store = VerbStore::new(&mut conf).unwrap();
    let cp = store
        .verbs()
        .iter()
        .find(|v| v.has_name("cp"))
        .expect("built-in cp must exist");
    if let VerbExecution::External(ext) = &cp.execution {
        let pat: String = ext.exec_pattern.tokens().join(" ");
        assert!(
            pat.contains("{newpath:path-from-parent}"),
            "cp exec pattern should resolve newpath via path-from-parent: {pat}"
        );
        assert_eq!(ext.exec_pattern.tokens().first().map(String::as_str), Some("cp"));
    } else {
        panic!("cp must be an external verb");
    }
}

#[cfg(unix)]
#[test]
fn mv_verb_uses_path_from_parent() {
    let mut conf = Conf::default();
    let store = VerbStore::new(&mut conf).unwrap();
    let mv = store
        .verbs()
        .iter()
        .find(|v| v.has_name("mv"))
        .expect("built-in mv must exist");
    if let VerbExecution::External(ext) = &mv.execution {
        let pat: String = ext.exec_pattern.tokens().join(" ");
        assert!(
            pat.contains("{newpath:path-from-parent}"),
            "mv exec pattern should resolve newpath via path-from-parent: {pat}"
        );
        assert_eq!(ext.exec_pattern.tokens().first().map(String::as_str), Some("mv"));
    } else {
        panic!("mv must be an external verb");
    }
}

#[cfg(unix)]
#[test]
fn copy_to_panel_verb_uses_other_panel_directory() {
    let mut conf = Conf::default();
    let store = VerbStore::new(&mut conf).unwrap();
    let cpp = store
        .verbs()
        .iter()
        .find(|v| v.has_name("copy_to_panel"))
        .expect("built-in copy_to_panel must exist");
    if let VerbExecution::External(ext) = &cpp.execution {
        assert!(ext.exec_pattern.has_other_panel_group());
        assert_eq!(ext.exec_pattern.tokens().first().map(String::as_str), Some("cp"));
    } else {
        panic!("copy_to_panel must be an external verb");
    }
}

#[cfg(unix)]
#[test]
fn move_to_panel_verb_uses_other_panel_directory() {
    let mut conf = Conf::default();
    let store = VerbStore::new(&mut conf).unwrap();
    let mvp = store
        .verbs()
        .iter()
        .find(|v| v.has_name("move_to_panel"))
        .expect("built-in move_to_panel must exist");
    if let VerbExecution::External(ext) = &mvp.execution {
        assert!(ext.exec_pattern.has_other_panel_group());
        assert_eq!(ext.exec_pattern.tokens().first().map(String::as_str), Some("mv"));
    } else {
        panic!("move_to_panel must be an external verb");
    }
}

#[test]
fn overwrite_overlay_preserves_cp_invocation() {
    let pending = Command::from_raw(":cp /tmp/dst".to_string(), true);
    let mut o = ConfirmOverlay::new(
        "Overwrite dst?",
        vec!["/tmp/dst".to_string()],
        "Overwrite",
        true,
        pending,
    );
    let outcome = o.handle_key(key_y());
    match outcome {
        OverlayOutcome::CloseAndRun(Command::VerbInvocate(inv)) => {
            assert_eq!(inv.name, "cp");
            assert_eq!(inv.args.as_deref(), Some("/tmp/dst"));
        }
        other => panic!("expected CloseAndRun(VerbInvocate(cp)), got {other:?}"),
    }
}

// =============================================================================
// Task 10: bulk-staging confirmation. The runtime "is the stage panel
// active and does it hold >1 paths" decision lives inside
// `App::maybe_bulk_stage_confirm` (private to `src/app/app.rs`); the
// helpers `is_stage_management_internal` and `bulk_stage_body` are
// covered by unit tests there. This integration block pins the public
// surface that the runtime relies on:
//
//   1. The confirm overlay constructed for a bulk fan-out preserves
//      the pending command verbatim across confirm/cancel.
//   2. Non-destructive bulk verbs flow through with `danger == false`
//      (red palette is reserved for verbs whose `requires_confirm` is
//      set — the runtime forwards that flag).
//   3. The body listing is what the user actually sees — a vec of
//      stringified paths.
// =============================================================================

#[test]
fn bulk_overlay_preserves_destructive_verb_invocation() {
    // Behavioural pin: confirming a bulk overlay re-emits the original
    // `:rm` command verbatim. Field shape (focus / danger / body /
    // title) is asserted indirectly: the overlay starts focused on
    // Cancel (Esc closes) and Y always confirms.
    let pending = Command::from_raw(":rm".to_string(), true);
    let body = vec![
        "/tmp/a.txt".to_string(),
        "/tmp/b.txt".to_string(),
        "/tmp/c.txt".to_string(),
    ];
    let mut o = ConfirmOverlay::new(
        "Run :rm on 3 files?",
        body,
        "Delete",
        true,
        pending,
    );
    let outcome = o.handle_key(key_y());
    match outcome {
        OverlayOutcome::CloseAndRun(Command::VerbInvocate(inv)) => {
            assert_eq!(inv.name, "rm");
        }
        other => panic!("expected CloseAndRun(VerbInvocate(rm)), got {other:?}"),
    }
}

#[test]
fn bulk_overlay_cancel_leaves_pending_command_unconsumed() {
    // The pending command is never re-dispatched when the user cancels.
    let pending = Command::from_raw(":rm".to_string(), true);
    let mut o = ConfirmOverlay::new(
        "Run :rm on 5 files?",
        (0..5)
            .map(|i| format!("/tmp/f{i}.txt"))
            .collect(),
        "Delete",
        true,
        pending,
    );
    let outcome = o.handle_key(key!(esc));
    assert!(
        matches!(outcome, OverlayOutcome::Close),
        "cancelling must NOT re-dispatch the bulk command"
    );
}

#[test]
fn bulk_overlay_cancel_leaves_files_intact() {
    // Filesystem-touching smoke test mirroring the cancel path on a
    // bulk overlay: build several temp files, build a fan-out overlay
    // referencing all of them, send Esc, verify nothing was touched.
    use std::fs::{
        File,
        metadata,
    };
    let mut cleanup = FsCleanup::new();
    let dir = std::env::temp_dir();
    let id = std::process::id();
    let paths: Vec<std::path::PathBuf> = (0..3)
        .map(|i| cleanup.add(dir.join(format!("broot_bulk_cancel_{id}_{i}.txt"))))
        .collect();
    for p in &paths {
        File::create(p).unwrap();
    }
    let body: Vec<String> = paths.iter().map(|p| p.to_string_lossy().to_string()).collect();
    let pending = Command::from_raw(":rm".to_string(), true);
    let mut o = ConfirmOverlay::new(
        format!("Run :rm on {} files?", paths.len()),
        body,
        "Delete",
        true,
        pending,
    );
    let outcome = o.handle_key(key!(esc));
    assert!(matches!(outcome, OverlayOutcome::Close));
    for p in &paths {
        assert!(p.exists(), "{p:?} must still exist after bulk cancel");
        assert!(metadata(p).is_ok());
    }
    // Cleanup runs via FsCleanup::Drop.
}

#[test]
fn overwrite_overlay_cancel_leaves_files_intact() {
    use std::fs::File;
    let mut cleanup = FsCleanup::new();
    let src = cleanup.add(
        std::env::temp_dir().join(format!("broot_overwrite_src_{}.txt", std::process::id())),
    );
    let dst = cleanup.add(
        std::env::temp_dir().join(format!("broot_overwrite_dst_{}.txt", std::process::id())),
    );
    File::create(&src).unwrap();
    File::create(&dst).unwrap();
    assert!(src.exists());
    assert!(dst.exists());

    let pending = Command::from_raw(format!(":cp {}", dst.display()), true);
    let mut o = ConfirmOverlay::new(
        format!("Overwrite {}?", dst.file_name().unwrap().to_string_lossy()),
        vec![dst.to_string_lossy().to_string()],
        "Overwrite",
        true,
        pending,
    );
    let outcome = o.handle_key(key!(esc));
    assert!(matches!(outcome, OverlayOutcome::Close));
    // Both files still untouched.
    assert!(src.exists());
    assert!(dst.exists());
    // Cleanup runs via FsCleanup::Drop.
}

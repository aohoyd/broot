//! Reusable `$EDITOR` helper.
//!
//! Writes a string to a temp file, suspends broot's TUI state (raw mode +
//! alternate screen) the same way `Launchable::Program` does in
//! `src/launchable.rs:170-205`, runs the user's editor on the temp file,
//! restores the TUI state on every return path (including errors), then
//! reads the edited content back.
//!
//! Resolution order is `$VISUAL` then `$EDITOR`; empty strings are treated
//! as unset.
//!
//! `suffix` is forwarded to the temp file's filename so the editor's
//! filetype heuristics (e.g. `.vimrc` autocmds) can fire. The suffix MUST
//! include the leading dot — `tempfile::Builder` does not add one.

use {
    crokey::crossterm::{
        QueueableCommand,
        cursor,
        terminal::{
            self,
            EnterAlternateScreen,
            LeaveAlternateScreen,
        },
    },
    std::{
        fs::File,
        io::{
            self,
            IsTerminal,
            Read,
            Write,
            stderr,
        },
        process::Command,
    },
};

/// Edit `content` in the user's `$VISUAL`/`$EDITOR`. The `suffix` (which
/// must include the leading dot, e.g. `".broot-rename"`) is appended to
/// the temp file's name for editor filetype detection. Returns the
/// content of the temp file after the editor exits.
///
/// Returns `io::ErrorKind::NotFound` with the message
/// `"set $EDITOR to enable this feature"` when neither env var is set
/// (or both are empty).
///
/// Editor command parsing splits on whitespace, so `EDITOR="code --wait"`
/// works for the common case. Shell quoting is not honored — that's a
/// YAGNI rabbit hole.
pub fn edit_in_external(
    content: &str,
    suffix: &str,
) -> io::Result<String> {
    let editor = resolve_editor()?;

    // First whitespace token is the executable; remaining tokens are
    // forwarded as arguments before the temp-file path. This is a
    // deliberate first cut — shell-style quoting is out of scope.
    let mut parts = editor.split_whitespace();
    let exe = parts
        .next()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "set $EDITOR to enable this feature",
            )
        })?
        .to_string();
    let extra_args: Vec<String> = parts.map(|s| s.to_string()).collect();

    let mut temp = tempfile::Builder::new().suffix(suffix).tempfile()?;
    temp.write_all(content.as_bytes())?;
    temp.flush()?;

    // Toggle out of broot's TUI state. The Drop impl of `TerminalRestoreGuard`
    // re-enters it on every return path (success, `?`-early-return, panic).
    // The toggle and the guard both skip when stderr is not a TTY — required
    // to keep `cargo test` output clean (otherwise we'd spray escape codes
    // into the test harness's captured stderr).
    //
    // CRITICAL: the guard MUST be installed before any of the fallible
    // toggle steps run. If e.g. `disable_raw_mode()` fails after we've
    // already shown the cursor and left the alternate screen, we still
    // need the Drop impl to put the TUI back together on the `?`-return.
    let is_tty = stderr().is_terminal();
    let _guard = TerminalRestoreGuard { is_tty };
    if is_tty {
        let mut w = stderr();
        w.queue(cursor::Show)?;
        w.queue(LeaveAlternateScreen)?;
        terminal::disable_raw_mode()?;
        w.flush()?;
    }

    let status = Command::new(&exe)
        .args(&extra_args)
        .arg(temp.path())
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!("editor exited with {status}")));
    }

    let mut buf = String::new();
    File::open(temp.path())?.read_to_string(&mut buf)?;
    Ok(buf)
}

/// Resolve `$VISUAL` then `$EDITOR`. Empty strings are treated as unset.
fn resolve_editor() -> io::Result<String> {
    #[cfg(test)]
    {
        if let Some(s) = test_support::current_override() {
            return s.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "set $EDITOR to enable this feature",
                )
            });
        }
    }
    resolve_editor_from_env(
        std::env::var("VISUAL").ok(),
        std::env::var("EDITOR").ok(),
    )
}

/// Pure-function resolver: `$VISUAL` wins over `$EDITOR`; whitespace-only
/// strings are treated as unset. Factored out so tests can exercise the
/// precedence rules without touching the process environment.
fn resolve_editor_from_env(
    visual: Option<String>,
    editor: Option<String>,
) -> io::Result<String> {
    // Filter EACH variable independently before chaining so a
    // whitespace-only $VISUAL doesn't short-circuit `or` and mask a
    // valid $EDITOR. `Some("   ").or(Some("vim"))` returns `Some("   ")`
    // because `or` keeps the first Some — so a single trailing
    // `.filter()` after the chain would drop the whole thing.
    visual
        .filter(|s| !s.trim().is_empty())
        .or_else(|| editor.filter(|s| !s.trim().is_empty()))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "set $EDITOR to enable this feature",
            )
        })
}

/// Re-enters broot's TUI state on drop. No-op when stderr is not a TTY.
/// Errors during restore are intentionally swallowed — there is nowhere
/// to report them from a `Drop` impl, and the alternative is a panic
/// during stack unwinding.
struct TerminalRestoreGuard {
    is_tty: bool,
}

impl Drop for TerminalRestoreGuard {
    fn drop(&mut self) {
        if !self.is_tty {
            return;
        }
        let mut w = stderr();
        let _ = terminal::enable_raw_mode();
        let _ = w.queue(EnterAlternateScreen);
        let _ = w.queue(cursor::Hide);
        let _ = w.flush();
    }
}

#[cfg(test)]
mod test_support {
    use std::sync::Mutex;

    /// `Some(Some(s))` => use `s` as the resolved editor command.
    /// `Some(None)`    => force "unset" (resolver returns the documented Err).
    /// `None`          => no override; resolver falls through to env vars.
    static OVERRIDE: Mutex<Option<Option<String>>> = Mutex::new(None);

    /// Lock the override mutex even when it's been poisoned by a panic
    /// in a previous test. A poisoned lock is recoverable here — the
    /// guarded value is a simple `Option`, not a half-mutated invariant
    /// — and cascading "previous test panicked" failures hide the real
    /// failure.
    fn lock_override() -> std::sync::MutexGuard<'static, Option<Option<String>>> {
        OVERRIDE.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(super) fn current_override() -> Option<Option<String>> {
        lock_override().clone()
    }

    pub(crate) fn set_override(value: Option<String>) {
        *lock_override() = Some(value);
    }

    pub(crate) fn clear_override() {
        *lock_override() = None;
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        std::sync::Mutex,
    };

    // Tests share the editor override; serialize them so they can't race.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Acquire `TEST_LOCK` even if a previous test panicked while holding
    /// it. The guarded value is `()` so a poisoned mutex carries no
    /// damaged state — recovering avoids spurious cascade failures
    /// where every subsequent test panics on `unwrap()` of `Err(PoisonError)`.
    fn lock_test() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// RAII guard that clears the editor override on drop, including on
    /// panic-unwind paths. Without this, an assertion failure between
    /// `set_override` and `clear_override` leaves the shared `OVERRIDE`
    /// dirty for the next test (and the symptom is a hard-to-trace
    /// cross-test contamination).
    struct OverrideGuard;
    impl Drop for OverrideGuard {
        fn drop(&mut self) {
            test_support::clear_override();
        }
    }

    #[test]
    fn edit_in_external_with_noop_editor_round_trips_content() {
        let _g = lock_test();
        // A no-op editor (exits 0, writes nothing) leaves the temp file
        // unchanged, so the returned String must equal the original.
        // `true` lives at `/bin/true` on Linux and `/usr/bin/true` on
        // macOS — pick the first one that exists.
        let true_path = ["/bin/true", "/usr/bin/true"]
            .iter()
            .find(|p| std::path::Path::new(p).exists())
            .expect("no `true` binary found on this system");
        test_support::set_override(Some((*true_path).to_string()));
        let _override = OverrideGuard;
        let out = edit_in_external("hello", ".test")
            .expect("editor helper should succeed with the no-op editor");
        assert_eq!(out, "hello");
    }

    #[test]
    fn edit_in_external_returns_documented_err_when_editor_unset() {
        let _g = lock_test();
        // Force-unset via override so we don't depend on the process env
        // (the test harness may have $EDITOR set in CI).
        test_support::set_override(None);
        let _override = OverrideGuard;
        let err = edit_in_external("hello", ".test")
            .expect_err("expected Err when editor is unset");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert_eq!(err.to_string(), "set $EDITOR to enable this feature");
    }

    // -------------------------------------------------------------
    // resolve_editor_from_env: pure-function resolution rules
    // -------------------------------------------------------------

    #[test]
    fn resolve_prefers_visual_over_editor() {
        // Both set — VISUAL wins, EDITOR is ignored.
        let r = resolve_editor_from_env(
            Some("vim".to_string()),
            Some("nano".to_string()),
        );
        assert_eq!(r.expect("VISUAL must win"), "vim");
    }

    #[test]
    fn resolve_falls_back_to_editor_when_visual_unset() {
        let r = resolve_editor_from_env(None, Some("nano".to_string()));
        assert_eq!(r.expect("EDITOR must be returned"), "nano");
    }

    #[test]
    fn resolve_whitespace_visual_falls_back_to_editor() {
        // Regression pin: `Some("   ").or(Some("vim"))` returns
        // `Some("   ")` because `or` keeps the first Some. A single
        // trailing `.filter(non_blank)` would then drop the chain
        // entirely and return Err — silently dropping a perfectly valid
        // $EDITOR. We must filter each variable independently before
        // chaining so $VISUAL='   ' falls through to $EDITOR='vim'.
        let r = resolve_editor_from_env(
            Some("   ".to_string()),
            Some("vim".to_string()),
        );
        assert_eq!(r.expect("EDITOR must be returned when VISUAL is blank"), "vim");
    }

    #[test]
    fn resolve_rejects_whitespace_only_editor() {
        // Both set to whitespace — must return the documented "unset" Err
        // (not a whitespace string that would later fail to launch).
        let r = resolve_editor_from_env(
            Some("   ".to_string()),
            Some("\t\n".to_string()),
        );
        let err = r.expect_err("whitespace-only must count as unset");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn resolve_returns_err_when_both_unset() {
        let r = resolve_editor_from_env(None, None);
        let err = r.expect_err("unset must return documented Err");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    // -------------------------------------------------------------
    // edit_in_external: non-zero exit surfaces as io::Error::other
    // -------------------------------------------------------------

    #[test]
    fn edit_in_external_non_zero_exit_returns_error() {
        let _g = lock_test();
        // `false` exits with status 1 unconditionally. The helper must
        // map that into an io::Error rather than silently returning the
        // (untouched) temp-file content.
        let false_path = ["/bin/false", "/usr/bin/false"]
            .iter()
            .find(|p| std::path::Path::new(p).exists())
            .expect("no `false` binary found on this system");
        test_support::set_override(Some((*false_path).to_string()));
        let _override = OverrideGuard;
        let err = edit_in_external("hello", ".test")
            .expect_err("non-zero exit must surface as Err");
        assert!(
            err.to_string().contains("editor exited with"),
            "error message must explain the failure: {err}",
        );
    }
}

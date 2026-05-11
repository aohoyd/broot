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
    let is_tty = stderr().is_terminal();
    if is_tty {
        let mut w = stderr();
        w.queue(cursor::Show)?;
        w.queue(LeaveAlternateScreen)?;
        terminal::disable_raw_mode()?;
        w.flush()?;
    }
    let _guard = TerminalRestoreGuard { is_tty };

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
    std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .ok()
        .filter(|s| !s.trim().is_empty())
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

    pub(super) fn current_override() -> Option<Option<String>> {
        OVERRIDE.lock().unwrap().clone()
    }

    pub(crate) fn set_override(value: Option<String>) {
        *OVERRIDE.lock().unwrap() = Some(value);
    }

    pub(crate) fn clear_override() {
        *OVERRIDE.lock().unwrap() = None;
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

    #[test]
    fn edit_in_external_with_noop_editor_round_trips_content() {
        let _g = TEST_LOCK.lock().unwrap();
        // A no-op editor (exits 0, writes nothing) leaves the temp file
        // unchanged, so the returned String must equal the original.
        // `true` lives at `/bin/true` on Linux and `/usr/bin/true` on
        // macOS — pick the first one that exists.
        let true_path = ["/bin/true", "/usr/bin/true"]
            .iter()
            .find(|p| std::path::Path::new(p).exists())
            .expect("no `true` binary found on this system");
        test_support::set_override(Some((*true_path).to_string()));
        // (`set_override(Some(s))` => use `s`; `set_override(None)` => force-unset.)
        let result = edit_in_external("hello", ".test");
        test_support::clear_override();
        let out = result.expect("editor helper should succeed with the no-op editor");
        assert_eq!(out, "hello");
    }

    #[test]
    fn edit_in_external_returns_documented_err_when_editor_unset() {
        let _g = TEST_LOCK.lock().unwrap();
        // Force-unset via override so we don't depend on the process env
        // (the test harness may have $EDITOR set in CI).
        test_support::set_override(None);
        let result = edit_in_external("hello", ".test");
        test_support::clear_override();
        let err = result.expect_err("expected Err when editor is unset");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert_eq!(err.to_string(), "set $EDITOR to enable this feature");
    }
}

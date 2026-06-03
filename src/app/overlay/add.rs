//! Add modal — create file or directory.
//!
//! `AddOverlay` is a free-text input modal asking the user for a name
//! relative to a target directory. Trailing `/` semantically creates a
//! directory (mkdir-p style); otherwise a regular file (with any
//! intermediate dirs created via `create_dir_all`).
//!
//! Layout (centred on the screen):
//!
//! ```text
//! ╭─ New file or directory ──────────────────────╮
//! │ in: <target_dir>                             │
//! │ <input>▏                                     │
//! │ (trailing / creates a directory)             │
//! │  [ Cancel ]                     [ Create ]   │
//! ╰──────────────────────────────────────────────╯
//! ```

use {
    super::{
        ButtonHits,
        CellGetCloned,
        OverlayOutcome,
        OverlayState,
        io_err,
        truncate_to_width,
    },
    crate::{
        display::frame::{
            self,
            FrameStyle,
        },
        skin::StyleMap,
    },
    crokey::{
        KeyCombination,
        crossterm::{
            QueueableCommand,
            cursor,
            event::{
                KeyCode,
                KeyModifiers,
                MouseButton,
                MouseEvent,
                MouseEventKind,
            },
        },
        key,
    },
    std::{
        cell::Cell,
        fs,
        io::{
            self,
            Write,
        },
        path::{
            Component,
            Path,
            PathBuf,
        },
    },
    termimad::{
        Area,
        CompoundStyle,
    },
};

/// Which button currently has keyboard focus.
///
/// Crate-internal: the overlay's focus state is an implementation
/// detail of the overlay layer. `Cancel` is the safe default so a stray
/// mouse click won't accidentally create a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AddFocus {
    Cancel,
    Create,
}

/// A free-text overlay that asks for a filename relative to
/// `target_dir`. A trailing `/` in the input creates a directory
/// (mkdir-p style); otherwise a regular file is created with any
/// intermediate directories.
///
/// `cursor` is a byte index into `input` and tracks the insertion
/// position. For ASCII filenames this matches char index; for
/// multi-byte UTF-8 we step on char boundaries via `char_indices`.
pub struct AddOverlay {
    pub(crate) target_dir: PathBuf,
    pub(crate) input: String,
    pub(crate) cursor: usize,
    pub(crate) error: Option<String>,
    pub(crate) focus: AddFocus,
    /// Hit-rects for mouse routing — populated by `render` so
    /// `handle_mouse` can test clicks against them.
    pub(crate) button_hits: Cell<Option<ButtonHits>>,
}

impl AddOverlay {
    /// Build an `AddOverlay` rooted at `target_dir` with an empty input
    /// and Cancel focused.
    pub fn new(target_dir: PathBuf) -> Self {
        Self {
            target_dir,
            input: String::new(),
            cursor: 0,
            error: None,
            focus: AddFocus::Cancel,
            button_hits: Cell::new(None),
        }
    }

    /// Validate `self.input` against the rules in the plan. Returns an
    /// error string ready to render, or `None` if the input is valid.
    fn validation_error(&self) -> Option<String> {
        if self.input.is_empty() {
            return Some("name cannot be empty".to_string());
        }
        // Reject any absolute path. `is_absolute()` is platform-aware:
        // on POSIX it catches leading `/`; on Windows it also catches
        // drive-letter forms like `C:\foo` and UNC paths `\\server\share`,
        // which `target_dir.join(...)` would otherwise resolve to the
        // absolute path, escaping `target_dir`.
        if Path::new(&self.input).is_absolute() {
            return Some("name cannot be an absolute path".to_string());
        }
        if Path::new(&self.input)
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        {
            return Some("'..' not allowed".to_string());
        }
        None
    }

    /// Run validation + filesystem commit. On success returns
    /// `CloseAndFocus(full)`; on validation or IO failure stores the
    /// error in `self.error` and returns `Stay`.
    fn try_commit(&mut self) -> OverlayOutcome {
        if let Some(err) = self.validation_error() {
            self.error = Some(err);
            return OverlayOutcome::Stay;
        }

        // Compute target path. Trailing `/` means directory.
        let make_dir = self.input.ends_with('/');
        let trimmed = self.input.trim_end_matches('/');
        let full = self.target_dir.join(trimmed);

        // Refuse to clobber an existing entry. `fs::File::create` would
        // silently truncate any existing file at `full`, and
        // `create_dir_all` is a no-op on an existing directory. Neither
        // is what a user typing a name into a "new file" modal would
        // expect; surface it as an inline error and let them pick a
        // different name. This is also consistent with the codebase's
        // safety model — destructive operations require a confirm
        // overlay, and silent overwrite would bypass that policy.
        if full.exists() {
            self.error = Some("file or directory already exists".to_string());
            return OverlayOutcome::Stay;
        }

        let result: io::Result<()> = if make_dir {
            fs::create_dir_all(&full)
        } else {
            // Ensure the parent directory exists. `full.parent()`
            // returns `None` only for paths with no parent component
            // (e.g. the root `/`); we already rejected absolute paths
            // in validation, so falling back to `target_dir` is safe.
            let parent = full.parent().unwrap_or(&self.target_dir);
            fs::create_dir_all(parent)
                .and_then(|()| fs::File::create(&full).map(|_| ()))
        };

        match result {
            Ok(()) => OverlayOutcome::CloseAndFocus(full),
            Err(err) => {
                self.error = Some(format!("{err}"));
                OverlayOutcome::Stay
            }
        }
    }

    /// Find the byte index of the char that ends at `self.cursor`,
    /// i.e. the start of the char immediately before the cursor.
    /// Returns `None` if the cursor is at 0.
    fn prev_char_boundary(&self) -> Option<usize> {
        if self.cursor == 0 {
            return None;
        }
        self.input
            .char_indices()
            .rev()
            .find(|(i, _)| *i < self.cursor)
            .map(|(i, _)| i)
    }

    /// Find the byte index of the char immediately after the cursor.
    /// Returns `None` if the cursor is already at the string end.
    fn next_char_boundary(&self) -> Option<usize> {
        debug_assert!(
            self.input.is_char_boundary(self.cursor),
            "cursor must always sit on a char boundary",
        );
        self.input
            .char_indices()
            .find(|(i, _)| *i >= self.cursor)
            .map(|(i, c)| i + c.len_utf8())
    }
}

// =============================================================================
// OverlayState impl
// =============================================================================

impl OverlayState for AddOverlay {
    fn render<Wr: Write>(
        &self,
        w: &mut Wr,
        screen: Area,
        palette: &StyleMap,
    ) -> io::Result<()> {
        // ---- geometry ---------------------------------------------------
        // Target ~60 wide × 7 tall: top frame, "in:" line, input row,
        // hint row, blank row, button row, bottom frame.
        let area = frame::centered_rect(screen, 60, 7);
        if area.width < 8 || area.height < 5 {
            // Too small to draw anything sensible — bail. Matches the
            // confirm overlay's bail-out at `confirm.rs:164-168`.
            return Ok(());
        }

        // ---- background clear ------------------------------------------
        let bg = &palette.default;
        for y in area.top..(area.top + area.height) {
            w.queue(cursor::MoveTo(area.left, y))?;
            for _ in 0..area.width {
                bg.queue(w, ' ').map_err(io_err)?;
            }
        }

        // ---- frame + title ---------------------------------------------
        let style = FrameStyle::rounded();
        frame::draw_frame(w, area.clone(), palette, &style)?;
        frame::draw_frame_title(w, area.clone(), palette, "New file or directory", false)?;

        // ---- body geometry ---------------------------------------------
        let inner_left = area.left + 2;
        let inner_width = area.width.saturating_sub(4) as usize;
        let body_style = &palette.default;

        // ---- target_dir line -------------------------------------------
        let dir_label = format!("in: {}", self.target_dir.display());
        w.queue(cursor::MoveTo(inner_left, area.top + 1))?;
        body_style
            .queue_str(w, truncate_to_width(&dir_label, inner_width))
            .map_err(io_err)?;

        // ---- input row -------------------------------------------------
        // Painted with `palette.input` so it visually reads as an input
        // field. After the input string we paint a single cursor glyph
        // (`▏`) — purely visual; no real terminal cursor is moved.
        let input_style = &palette.input;
        w.queue(cursor::MoveTo(inner_left, area.top + 2))?;
        // Reserve 1 column for the cursor glyph.
        let input_budget = inner_width.saturating_sub(1);
        let input_display = truncate_to_width(&self.input, input_budget);
        input_style
            .queue_str(w, &input_display)
            .map_err(io_err)?;
        // Cursor glyph — visual only; positioned at the end of the
        // (possibly truncated) input.
        input_style.queue(w, '▏').map_err(io_err)?;

        // ---- hint / error row ------------------------------------------
        let (hint_text, hint_style): (String, &CompoundStyle) = match &self.error {
            Some(err) => (err.clone(), &palette.file_error),
            None => (
                "(trailing / creates a directory)".to_string(),
                &palette.default,
            ),
        };
        w.queue(cursor::MoveTo(inner_left, area.top + 3))?;
        hint_style
            .queue_str(w, truncate_to_width(&hint_text, inner_width))
            .map_err(io_err)?;

        // ---- button row -------------------------------------------------
        // Row sits at area.top + area.height - 2 (one above the bottom
        // border) — same convention as confirm.rs.
        let button_row_y = area.top + area.height - 2;
        let half = area.width / 2;

        let cancel_text = " [ Cancel ] ";
        let create_text = " [ Create ] ";

        let cancel_x = area.left + 1;
        let cancel_w = (cancel_text.chars().count() as u16).min(half.saturating_sub(2));
        let confirm_w =
            (create_text.chars().count() as u16).min(area.width.saturating_sub(half + 2));
        let confirm_x = area.left + area.width - 1 - confirm_w;

        let cancel_focused = matches!(self.focus, AddFocus::Cancel);
        let create_focused = matches!(self.focus, AddFocus::Create);

        let cancel_style: &CompoundStyle = if cancel_focused {
            &palette.selected_line
        } else {
            &palette.default
        };
        let create_style: &CompoundStyle = if create_focused {
            &palette.selected_line
        } else {
            &palette.default
        };

        w.queue(cursor::MoveTo(cancel_x, button_row_y))?;
        cancel_style
            .queue_str(w, truncate_to_width(cancel_text, cancel_w as usize))
            .map_err(io_err)?;

        w.queue(cursor::MoveTo(confirm_x, button_row_y))?;
        create_style
            .queue_str(w, truncate_to_width(create_text, confirm_w as usize))
            .map_err(io_err)?;

        // Cache hit-rects for mouse routing.
        let cancel_rect = Area::new(cancel_x, button_row_y, cancel_w, 1);
        let confirm_rect = Area::new(confirm_x, button_row_y, confirm_w, 1);
        self.button_hits.set(Some(ButtonHits {
            cancel: cancel_rect,
            confirm: confirm_rect,
        }));

        Ok(())
    }

    fn handle_key(
        &mut self,
        key: KeyCombination,
    ) -> OverlayOutcome {
        if key == key!(esc) || key == key!(ctrl - c) {
            return OverlayOutcome::Close;
        }

        // Enter always tries to commit, regardless of focus. The buttons
        // exist for mouse users; keyboard users live in the input field
        // and Enter is the natural "submit" key. Cancel via Esc/Ctrl-C
        // or a mouse click on the Cancel button.
        if key == key!(enter) {
            return self.try_commit();
        }

        // Note: arrow keys are *not* focus-toggles here (unlike
        // confirm.rs) because they must move the text cursor.
        if key == key!(tab) {
            self.focus = match self.focus {
                AddFocus::Cancel => AddFocus::Create,
                AddFocus::Create => AddFocus::Cancel,
            };
            return OverlayOutcome::Stay;
        }

        if key == key!(left) {
            if let Some(prev) = self.prev_char_boundary() {
                self.cursor = prev;
            }
            return OverlayOutcome::Stay;
        }
        if key == key!(right) {
            if let Some(next) = self.next_char_boundary() {
                self.cursor = next.min(self.input.len());
            }
            return OverlayOutcome::Stay;
        }
        if key == key!(home) {
            self.cursor = 0;
            return OverlayOutcome::Stay;
        }
        if key == key!(end) {
            self.cursor = self.input.len();
            return OverlayOutcome::Stay;
        }

        if key == key!(backspace) {
            if let Some(prev) = self.prev_char_boundary() {
                self.input.remove(prev);
                self.cursor = prev;
            }
            return OverlayOutcome::Stay;
        }

        if let Some(c) = key_to_printable_char(&key) {
            // Defensive: only insert at a valid char boundary. `cursor`
            // is maintained at a boundary by every path above, so this
            // is essentially a no-op in practice — but it pins the
            // invariant.
            if self.cursor <= self.input.len() && self.input.is_char_boundary(self.cursor) {
                self.input.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            return OverlayOutcome::Stay;
        }

        // Anything else: consumed silently.
        OverlayOutcome::Stay
    }

    fn handle_mouse(
        &mut self,
        ev: MouseEvent,
    ) -> OverlayOutcome {
        if !matches!(ev.kind, MouseEventKind::Down(MouseButton::Left)) {
            return OverlayOutcome::Stay;
        }
        let Some(hits) = self.button_hits.get_cloned() else {
            return OverlayOutcome::Stay;
        };
        if hits.confirm.contains(ev.column, ev.row) {
            return self.try_commit();
        }
        if hits.cancel.contains(ev.column, ev.row) {
            return OverlayOutcome::Close;
        }
        OverlayOutcome::Stay
    }
}

// =============================================================================
// helpers
// =============================================================================

/// Extract a printable character from a `KeyCombination`. Modifier-
/// bearing combos other than plain Shift are rejected so e.g. `Ctrl-a`
/// doesn't get treated as the letter `a`. Control chars are also
/// rejected — those should never enter `input`.
fn key_to_printable_char(key: &KeyCombination) -> Option<char> {
    if !key.modifiers.is_empty() && key.modifiers != KeyModifiers::SHIFT {
        return None;
    }
    match key.codes.first() {
        KeyCode::Char(c) if !c.is_control() => Some(*c),
        _ => None,
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make() -> AddOverlay {
        AddOverlay::new(PathBuf::from("/tmp/sample"))
    }

    fn render_to_string(o: &AddOverlay) -> String {
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 80, 24);
        o.render(&mut wbuf, screen, &palette).unwrap();
        let bytes = wbuf.buffer().to_vec();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn mouse_at(x: u16, y: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }
    }

    // ---- construction ---------------------------------------------------

    #[test]
    fn new_has_safe_defaults() {
        let o = make();
        assert_eq!(o.input, "");
        assert_eq!(o.cursor, 0);
        assert!(o.error.is_none());
        assert_eq!(o.focus, AddFocus::Cancel);
        assert!(o.button_hits.take().is_none());
    }

    // ---- render shape ---------------------------------------------------

    #[test]
    fn render_writes_corners() {
        let o = make();
        let s = render_to_string(&o);
        assert!(s.contains('╭'), "missing top-left corner in render bytes: {s:?}");
        assert!(s.contains('╮'), "missing top-right corner in render bytes");
        assert!(s.contains('╰'), "missing bottom-left corner in render bytes");
        assert!(s.contains('╯'), "missing bottom-right corner in render bytes");
    }

    #[test]
    fn render_writes_title() {
        let o = make();
        let s = render_to_string(&o);
        assert!(
            s.contains("New file or directory"),
            "missing title in render bytes: {s:?}",
        );
    }

    #[test]
    fn render_writes_target_dir() {
        let o = make();
        let s = render_to_string(&o);
        assert!(
            s.contains("in: /tmp/sample"),
            "missing 'in:' line in render bytes: {s:?}",
        );
    }

    #[test]
    fn render_writes_cancel_and_create_labels() {
        let o = make();
        let s = render_to_string(&o);
        assert!(s.contains("Cancel"), "missing Cancel label in render bytes");
        assert!(s.contains("Create"), "missing Create label in render bytes");
    }

    #[test]
    fn render_writes_default_hint() {
        let o = make();
        let s = render_to_string(&o);
        assert!(
            s.contains("trailing / creates a directory"),
            "missing default hint in render bytes: {s:?}",
        );
    }

    #[test]
    fn render_error_replaces_hint() {
        let mut o = make();
        o.error = Some("permission denied".to_string());
        let s = render_to_string(&o);
        assert!(
            s.contains("permission denied"),
            "missing error text in render bytes: {s:?}",
        );
        assert!(
            !s.contains("trailing / creates a directory"),
            "hint must be replaced by error: {s:?}",
        );
    }

    #[test]
    fn render_caches_button_hits() {
        let o = make();
        assert!(o.button_hits.get_cloned().is_none());
        let _ = render_to_string(&o);
        let hits = o
            .button_hits
            .get_cloned()
            .expect("hits should be populated after render");
        assert!(hits.cancel.width > 0, "cancel hit-rect must have width");
        assert!(hits.confirm.width > 0, "create hit-rect must have width");
        let cancel_right = hits.cancel.left + hits.cancel.width;
        assert!(
            cancel_right <= hits.confirm.left,
            "cancel and create rects overlap: {hits:?}",
        );
    }

    #[test]
    fn render_input_string_appears_in_bytes() {
        let mut o = make();
        o.input = "hello.txt".to_string();
        o.cursor = o.input.len();
        let s = render_to_string(&o);
        assert!(
            s.contains("hello.txt"),
            "missing input text in render bytes: {s:?}",
        );
    }

    #[test]
    fn render_too_small_is_noop() {
        // Below the 8x5 bail-out threshold the overlay should write
        // nothing — pinning the bail behaviour like confirm.rs does.
        let o = make();
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        // 6 wide × 4 tall: centered_rect clamps to that, well below the
        // 8x5 minimum.
        let screen = Area::new(0, 0, 6, 4);
        o.render(&mut wbuf, screen, &palette).unwrap();
        let bytes = wbuf.buffer().to_vec();
        let s = String::from_utf8_lossy(&bytes);
        assert!(!s.contains('╭'), "tiny terminal should bail before drawing frame");
    }

    // ---- key dispatch: editing -----------------------------------------

    #[test]
    fn char_insertion_appends_at_end() {
        let mut o = make();
        let r = o.handle_key(key!('a'));
        assert!(matches!(r, OverlayOutcome::Stay));
        assert_eq!(o.input, "a");
        assert_eq!(o.cursor, 1);
        let _ = o.handle_key(key!('b'));
        let _ = o.handle_key(key!('c'));
        assert_eq!(o.input, "abc");
        assert_eq!(o.cursor, 3);
    }

    #[test]
    fn char_insertion_at_cursor_mid_string() {
        let mut o = make();
        for c in "ac".chars() {
            let _ = o.handle_key(KeyCombination::from(KeyCode::Char(c)));
        }
        // Move cursor between `a` and `c`.
        o.cursor = 1;
        let _ = o.handle_key(key!('b'));
        assert_eq!(o.input, "abc");
        assert_eq!(o.cursor, 2);
    }

    #[test]
    fn slash_is_a_valid_printable_char() {
        let mut o = make();
        for c in "ab".chars() {
            let _ = o.handle_key(KeyCombination::from(KeyCode::Char(c)));
        }
        let _ = o.handle_key(key!('/'));
        assert_eq!(o.input, "ab/");
        assert_eq!(o.cursor, 3);
    }

    #[test]
    fn backspace_deletes_char_before_cursor() {
        let mut o = make();
        for c in "abc".chars() {
            let _ = o.handle_key(KeyCombination::from(KeyCode::Char(c)));
        }
        let _ = o.handle_key(key!(backspace));
        assert_eq!(o.input, "ab");
        assert_eq!(o.cursor, 2);
    }

    #[test]
    fn backspace_at_zero_is_noop() {
        let mut o = make();
        let r = o.handle_key(key!(backspace));
        assert!(matches!(r, OverlayOutcome::Stay));
        assert_eq!(o.input, "");
        assert_eq!(o.cursor, 0);
    }

    #[test]
    fn backspace_mid_string_deletes_correct_char() {
        let mut o = make();
        for c in "abc".chars() {
            let _ = o.handle_key(KeyCombination::from(KeyCode::Char(c)));
        }
        o.cursor = 2; // between `b` and `c`.
        let _ = o.handle_key(key!(backspace));
        assert_eq!(o.input, "ac");
        assert_eq!(o.cursor, 1);
    }

    #[test]
    fn left_arrow_moves_cursor_back() {
        let mut o = make();
        for c in "ab".chars() {
            let _ = o.handle_key(KeyCombination::from(KeyCode::Char(c)));
        }
        let _ = o.handle_key(key!(left));
        assert_eq!(o.cursor, 1);
        let _ = o.handle_key(key!(left));
        assert_eq!(o.cursor, 0);
    }

    #[test]
    fn left_arrow_clamps_at_zero() {
        let mut o = make();
        let _ = o.handle_key(key!(left));
        assert_eq!(o.cursor, 0);
    }

    #[test]
    fn right_arrow_moves_cursor_forward() {
        let mut o = make();
        for c in "ab".chars() {
            let _ = o.handle_key(KeyCombination::from(KeyCode::Char(c)));
        }
        o.cursor = 0;
        let _ = o.handle_key(key!(right));
        assert_eq!(o.cursor, 1);
        let _ = o.handle_key(key!(right));
        assert_eq!(o.cursor, 2);
    }

    #[test]
    fn right_arrow_clamps_at_end() {
        let mut o = make();
        for c in "ab".chars() {
            let _ = o.handle_key(KeyCombination::from(KeyCode::Char(c)));
        }
        // cursor already at end (2).
        let _ = o.handle_key(key!(right));
        assert_eq!(o.cursor, 2);
    }

    #[test]
    fn home_jumps_to_start() {
        let mut o = make();
        for c in "abc".chars() {
            let _ = o.handle_key(KeyCombination::from(KeyCode::Char(c)));
        }
        let _ = o.handle_key(key!(home));
        assert_eq!(o.cursor, 0);
    }

    #[test]
    fn end_jumps_to_end() {
        let mut o = make();
        for c in "abc".chars() {
            let _ = o.handle_key(KeyCombination::from(KeyCode::Char(c)));
        }
        o.cursor = 0;
        let _ = o.handle_key(key!(end));
        assert_eq!(o.cursor, 3);
    }

    #[test]
    fn cursor_movement_with_multi_byte_chars() {
        // `é` is 2 UTF-8 bytes (0xC3 0xA9); `中` is 3 bytes. The cursor
        // is a byte index — `prev_char_boundary` / `next_char_boundary`
        // must hop over the entire codepoint, not break a char in half
        // (which would later panic in `input.remove(prev)`).
        let mut o = make();
        for c in "é中a".chars() {
            let _ = o.handle_key(KeyCombination::from(KeyCode::Char(c)));
        }
        // After insertion: input = "é中a" (2 + 3 + 1 = 6 bytes), cursor at 6.
        assert_eq!(o.input, "é中a");
        assert_eq!(o.cursor, 6);

        // Left moves to start of `a` (byte 5).
        let _ = o.handle_key(key!(left));
        assert_eq!(o.cursor, 5);
        // Left again moves to start of `中` (byte 2).
        let _ = o.handle_key(key!(left));
        assert_eq!(o.cursor, 2);
        // Left again moves to start of `é` (byte 0).
        let _ = o.handle_key(key!(left));
        assert_eq!(o.cursor, 0);

        // Right moves past `é` (byte 2).
        let _ = o.handle_key(key!(right));
        assert_eq!(o.cursor, 2);
        // Right moves past `中` (byte 5).
        let _ = o.handle_key(key!(right));
        assert_eq!(o.cursor, 5);
    }

    #[test]
    fn backspace_with_multi_byte_chars_removes_full_codepoint() {
        // Regression pin: backspacing must remove the whole codepoint
        // (2 bytes for `é`), not just a trailing byte — `input.remove`
        // panics if asked to slice mid-codepoint.
        let mut o = make();
        for c in "aé".chars() {
            let _ = o.handle_key(KeyCombination::from(KeyCode::Char(c)));
        }
        assert_eq!(o.input, "aé");
        assert_eq!(o.cursor, 3);
        let _ = o.handle_key(key!(backspace));
        assert_eq!(o.input, "a");
        assert_eq!(o.cursor, 1);
    }

    // ---- key dispatch: control -----------------------------------------

    #[test]
    fn tab_toggles_focus() {
        let mut o = make();
        assert_eq!(o.focus, AddFocus::Cancel);
        let r = o.handle_key(key!(tab));
        assert!(matches!(r, OverlayOutcome::Stay));
        assert_eq!(o.focus, AddFocus::Create);
        let _ = o.handle_key(key!(tab));
        assert_eq!(o.focus, AddFocus::Cancel);
    }

    #[test]
    fn arrow_keys_do_not_toggle_focus() {
        // Pin behaviour: in an input-bearing modal, arrows move the
        // text cursor, *not* focus. This is the divergence point from
        // `ConfirmOverlay` and we want it tested explicitly.
        let mut o = make();
        let _ = o.handle_key(key!(left));
        assert_eq!(o.focus, AddFocus::Cancel);
        let _ = o.handle_key(key!(right));
        assert_eq!(o.focus, AddFocus::Cancel);
    }

    #[test]
    fn esc_closes() {
        let mut o = make();
        let r = o.handle_key(key!(esc));
        assert!(matches!(r, OverlayOutcome::Close));
    }

    #[test]
    fn ctrl_c_closes() {
        let mut o = make();
        let r = o.handle_key(key!(ctrl - c));
        assert!(matches!(r, OverlayOutcome::Close));
    }

    #[test]
    fn ctrl_a_is_ignored_not_inserted() {
        // Ctrl-a is a control combo; it must not insert `a` into the
        // input or do any cursor-move (we'd need separate plumbing for
        // emacs-style line editing).
        let mut o = make();
        let _ = o.handle_key(key!(ctrl - a));
        assert_eq!(o.input, "");
        assert_eq!(o.cursor, 0);
    }

    #[test]
    fn shift_letter_inserts_uppercase() {
        // Plain Shift modifier is allowed; produces uppercase.
        let mut o = make();
        let _ = o.handle_key(key!(shift - 'a'));
        assert_eq!(o.input, "A");
        assert_eq!(o.cursor, 1);
    }

    // ---- validation -----------------------------------------------------

    #[test]
    fn empty_input_rejected_by_enter() {
        let mut o = make();
        let r = o.handle_key(key!(enter));
        assert!(matches!(r, OverlayOutcome::Stay));
        assert_eq!(o.error.as_deref(), Some("name cannot be empty"));
    }

    #[test]
    fn leading_slash_rejected() {
        let mut o = make();
        o.input = "/etc/passwd".to_string();
        o.cursor = o.input.len();
        let r = o.handle_key(key!(enter));
        assert!(matches!(r, OverlayOutcome::Stay));
        assert_eq!(
            o.error.as_deref(),
            Some("name cannot be an absolute path"),
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_drive_absolute_rejected() {
        let mut o = make();
        o.input = "C:\\Windows\\foo".to_string();
        o.cursor = o.input.len();
        let r = o.handle_key(key!(enter));
        assert!(matches!(r, OverlayOutcome::Stay));
        assert_eq!(
            o.error.as_deref(),
            Some("name cannot be an absolute path"),
        );
    }

    #[test]
    fn parent_dir_component_rejected() {
        let mut o = make();
        o.input = "../escape.txt".to_string();
        o.cursor = o.input.len();
        let r = o.handle_key(key!(enter));
        assert!(matches!(r, OverlayOutcome::Stay));
        assert_eq!(o.error.as_deref(), Some("'..' not allowed"));
    }

    #[test]
    fn parent_dir_nested_component_rejected() {
        // The check must catch `..` as any component, not just at the
        // start.
        let mut o = make();
        o.input = "foo/../bar".to_string();
        o.cursor = o.input.len();
        let r = o.handle_key(key!(enter));
        assert!(matches!(r, OverlayOutcome::Stay));
        assert_eq!(o.error.as_deref(), Some("'..' not allowed"));
    }

    // ---- mouse routing --------------------------------------------------

    #[test]
    fn mouse_on_cancel_closes() {
        let mut o = make();
        let _ = render_to_string(&o);
        let hits = o.button_hits.get_cloned().unwrap();
        let cx = hits.cancel.left + hits.cancel.width / 2;
        let cy = hits.cancel.top;
        let r = o.handle_mouse(mouse_at(cx, cy));
        assert!(matches!(r, OverlayOutcome::Close));
    }

    #[test]
    fn mouse_on_create_with_empty_input_stays_and_sets_error() {
        let mut o = make();
        let _ = render_to_string(&o);
        let hits = o.button_hits.get_cloned().unwrap();
        let cx = hits.confirm.left + hits.confirm.width / 2;
        let cy = hits.confirm.top;
        let r = o.handle_mouse(mouse_at(cx, cy));
        assert!(matches!(r, OverlayOutcome::Stay));
        assert!(o.error.is_some());
    }

    #[test]
    fn mouse_off_buttons_stays() {
        let mut o = make();
        let _ = render_to_string(&o);
        let r = o.handle_mouse(mouse_at(0, 0));
        assert!(matches!(r, OverlayOutcome::Stay));
    }

    #[test]
    fn mouse_non_left_click_stays() {
        let mut o = make();
        let _ = render_to_string(&o);
        let hits = o.button_hits.get_cloned().unwrap();
        let cx = hits.cancel.left + hits.cancel.width / 2;
        let cy = hits.cancel.top;
        let ev = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: cx,
            row: cy,
            modifiers: KeyModifiers::NONE,
        };
        let r = o.handle_mouse(ev);
        assert!(matches!(r, OverlayOutcome::Stay));
    }

    #[test]
    fn mouse_before_render_stays() {
        let mut o = make();
        let r = o.handle_mouse(mouse_at(40, 12));
        assert!(matches!(r, OverlayOutcome::Stay));
    }

    // ---- filesystem commit ---------------------------------------------

    #[test]
    fn commit_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut o = AddOverlay::new(tmp.path().to_path_buf());
        o.input = "foo.txt".to_string();
        o.cursor = o.input.len();
        let r = o.handle_key(key!(enter));
        let expected = tmp.path().join("foo.txt");
        match r {
            OverlayOutcome::CloseAndFocus(p) => assert_eq!(p, expected),
            other => panic!("expected CloseAndFocus, got {other:?}"),
        }
        assert!(expected.exists(), "file must exist on disk");
        assert!(expected.is_file(), "must be a regular file");
        assert!(o.error.is_none());
    }

    #[test]
    fn commit_creates_directory_with_trailing_slash() {
        let tmp = tempfile::tempdir().unwrap();
        let mut o = AddOverlay::new(tmp.path().to_path_buf());
        o.input = "bar/".to_string();
        o.cursor = o.input.len();
        let r = o.handle_key(key!(enter));
        let expected = tmp.path().join("bar");
        match r {
            OverlayOutcome::CloseAndFocus(p) => assert_eq!(p, expected),
            other => panic!("expected CloseAndFocus, got {other:?}"),
        }
        assert!(expected.exists());
        assert!(expected.is_dir(), "must be a directory");
    }

    #[test]
    fn commit_creates_nested_file_with_intermediate_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let mut o = AddOverlay::new(tmp.path().to_path_buf());
        o.input = "nested/deeper/file.txt".to_string();
        o.cursor = o.input.len();
        let r = o.handle_key(key!(enter));
        let expected = tmp.path().join("nested/deeper/file.txt");
        match r {
            OverlayOutcome::CloseAndFocus(p) => assert_eq!(p, expected),
            other => panic!("expected CloseAndFocus, got {other:?}"),
        }
        assert!(expected.exists());
        assert!(expected.is_file());
        assert!(tmp.path().join("nested/deeper").is_dir());
    }

    #[test]
    fn commit_existing_file_rejected_with_error() {
        // Pin: if the target already exists, the modal refuses to
        // clobber it. The user gets an inline error and a chance to
        // pick a different name. Silent overwrite would bypass the
        // codebase's "destructive ops need a confirm overlay" policy.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("existing.txt");
        let original = b"original contents that must be preserved";
        std::fs::write(&path, original).unwrap();
        let mut o = AddOverlay::new(tmp.path().to_path_buf());
        o.input = "existing.txt".to_string();
        o.cursor = o.input.len();
        let r = o.handle_key(key!(enter));
        assert!(matches!(r, OverlayOutcome::Stay));
        assert_eq!(
            o.error.as_deref(),
            Some("file or directory already exists"),
        );
        // Existing file contents must be untouched.
        let after = std::fs::read(&path).unwrap();
        assert_eq!(after, original, "existing file must not be overwritten");
    }

    #[test]
    fn commit_existing_directory_rejected_with_error() {
        // Same policy for an existing directory: `mkdir foo/` on a
        // present `foo` would otherwise be a silent no-op that the
        // user reads as success.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("existing_dir");
        std::fs::create_dir(&path).unwrap();
        let mut o = AddOverlay::new(tmp.path().to_path_buf());
        o.input = "existing_dir/".to_string();
        o.cursor = o.input.len();
        let r = o.handle_key(key!(enter));
        assert!(matches!(r, OverlayOutcome::Stay));
        assert_eq!(
            o.error.as_deref(),
            Some("file or directory already exists"),
        );
        assert!(path.is_dir(), "existing directory must remain on disk");
    }

    #[test]
    fn commit_failure_sets_error_and_stays() {
        // target_dir is a non-existent path inside a read-only location
        // — actually, easier: the target_dir itself doesn't exist, and
        // the filename contains a NUL byte which always errors on POSIX.
        // But `Path::new` accepts NUL until the `fs::File::create` call.
        // Use a NUL in the filename: that will error in `File::create`
        // / `create_dir_all` reliably.
        let tmp = tempfile::tempdir().unwrap();
        let mut o = AddOverlay::new(tmp.path().to_path_buf());
        o.input = "bad\0name".to_string();
        o.cursor = o.input.len();
        let r = o.handle_key(key!(enter));
        assert!(matches!(r, OverlayOutcome::Stay));
        assert!(o.error.is_some(), "filesystem error should populate error");
    }
}

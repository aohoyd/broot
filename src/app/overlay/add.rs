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
//!
//! This task (Task 2) lands the struct, the render path, and the enum
//! wiring. `handle_key` and `handle_mouse` are stubbed to `Stay` so the
//! variant compiles and routing tests pass — Task 3 wires real input.

use {
    super::{
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
            event::MouseEvent,
        },
    },
    std::{
        cell::Cell,
        io::{
            self,
            Write,
        },
        path::PathBuf,
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
/// Enter does not create a file.
///
/// `Create` is unused until Task 3 wires Tab-focus-toggle and Enter
/// dispatch; render already styles both branches so the variant must
/// exist now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum AddFocus {
    Cancel,
    Create,
}

/// Hit-test rectangles for the two buttons. Recomputed on every render
/// and consulted by `handle_mouse` (Task 3) to translate clicks into
/// outcomes.
///
/// Intentionally a local copy of the same shape used by
/// `ConfirmOverlay::ButtonHits` rather than a shared import: the cost
/// of cross-module visibility plumbing for a 4-line struct is higher
/// than the cost of the duplication.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct ButtonHits {
    pub(crate) cancel: Area,
    pub(crate) confirm: Area,
}

/// A free-text overlay that asks for a filename relative to
/// `target_dir`. A trailing `/` in the input creates a directory
/// (mkdir-p style); otherwise a regular file is created with any
/// intermediate directories.
///
/// `cursor` is in `chars()` units and tracks the insertion position
/// inside `input`. Render currently paints the cursor glyph at the
/// tail of the (truncated) input; Task 3 will move the glyph to
/// `cursor` mid-input.
#[allow(dead_code)]
pub struct AddOverlay {
    pub(crate) target_dir: PathBuf,
    pub(crate) input: String,
    pub(crate) cursor: usize,
    pub(crate) error: Option<String>,
    pub(crate) focus: AddFocus,
    /// Hit-rects for mouse routing — populated by `render` so
    /// `handle_mouse` (Task 3) can test clicks against them.
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
        // (possibly truncated) input. Task 3 will paint the glyph at
        // `self.cursor` mid-input; this scaffold puts it at the tail.
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

        // Cache hit-rects for mouse routing (consumed by Task 3).
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
        _key: KeyCombination,
    ) -> OverlayOutcome {
        // Stub — Task 3 wires real input handling.
        OverlayOutcome::Stay
    }

    fn handle_mouse(
        &mut self,
        _ev: MouseEvent,
    ) -> OverlayOutcome {
        // Stub — Task 3 wires real mouse handling.
        OverlayOutcome::Stay
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
        assert!(o.button_hits.take().is_none());
        // `take` above emptied the cache; restore None and re-render so
        // we observe the actual render-time population.
        o.button_hits.set(None);
        let _ = render_to_string(&o);
        let hits = {
            let v = o.button_hits.take();
            o.button_hits.set(v.clone());
            v
        }
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
        o.cursor = o.input.chars().count();
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

    // ---- stubbed handlers (Task 3 will replace) -------------------------

    #[test]
    fn handle_key_stub_returns_stay() {
        let mut o = make();
        let r = o.handle_key(crokey::key!('a'));
        assert!(matches!(r, OverlayOutcome::Stay));
    }

    #[test]
    fn handle_mouse_stub_returns_stay() {
        use crokey::crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};
        let mut o = make();
        let ev = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        let r = o.handle_mouse(ev);
        assert!(matches!(r, OverlayOutcome::Stay));
    }
}

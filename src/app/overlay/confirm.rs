//! Yes/no confirmation overlay.
//!
//! `ConfirmOverlay` is a generic, reusable modal that asks the user to
//! confirm or cancel a pending [`Command`]. It is the foundation for
//! destructive-verb prompts (rm/trash), mv/cp overwrite, and bulk
//! staging confirmation.
//!
//! Layout (centred on the screen):
//!
//! ```text
//! ╭─ <title> ─────────────────────╮
//! │ <body line 1>                 │
//! │ <body line 2>                 │
//! │ …                             │
//! │  [ Cancel ]   [ <confirm> ]   │
//! ╰───────────────────────────────╯
//! ```
//!
//! The default focus is `Cancel` so an absent-minded `Enter` is safe.
//! `y` directly confirms; `n`, `Esc`, and `Ctrl-C` cancel. `Tab`, `←`,
//! and `→` toggle focus. `↑` / `↓` scroll the body when it overflows.

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
        command::Command,
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
                MouseButton,
                MouseEvent,
                MouseEventKind,
            },
        },
        key,
    },
    std::{
        cell::Cell,
        io::{
            self,
            Write,
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
/// detail of the overlay layer, not part of the public surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfirmFocus {
    Cancel,
    Confirm,
}

/// A yes/no confirmation modal.
///
/// `pending` is the command that fires when the user confirms;
/// `body` is the affected paths or any other context lines (vertical
/// scrollable when there are more than fit). `confirm_label` lets the
/// caller pick the verb shown on the confirm button (e.g. "Delete",
/// "Trash", "Overwrite"). `danger` switches the confirm button to a
/// red/error palette.
pub struct ConfirmOverlay {
    pub(crate) title: String,
    pub(crate) body: Vec<String>,
    pub(crate) confirm_label: String,
    pub(crate) danger: bool,
    pub(crate) pending: Command,
    pub(crate) focus: ConfirmFocus,
    pub(crate) scroll: u16,
    /// Hit-rects for mouse routing — populated by `render` so
    /// `handle_mouse` can test clicks against them. `Cell` is enough
    /// because the value is `Copy`-able as a small struct.
    button_hits: Cell<Option<ButtonHits>>,
}

impl ConfirmOverlay {
    /// Build a `ConfirmOverlay` with sensible defaults: focus on
    /// `Cancel`, scroll at 0, no cached hit-rects.
    pub fn new(
        title: impl Into<String>,
        body: Vec<String>,
        confirm_label: impl Into<String>,
        danger: bool,
        pending: Command,
    ) -> Self {
        Self {
            title: title.into(),
            body,
            confirm_label: confirm_label.into(),
            danger,
            pending,
            focus: ConfirmFocus::Cancel,
            scroll: 0,
            button_hits: Cell::new(None),
        }
    }

    /// Visible body height in rows for the given outer area, accounting
    /// for the 4 rows of chrome (top frame, bottom frame, button row,
    /// separator above buttons).
    fn visible_body_rows(area: Area) -> u16 {
        area.height.saturating_sub(4)
    }

    /// Maximum legal `scroll` value so the last visible row still
    /// shows real content rather than blank.
    fn max_scroll(
        body_len: usize,
        visible: u16,
    ) -> u16 {
        let body_len = body_len as u32;
        let visible = visible as u32;
        if body_len <= visible {
            0
        } else {
            (body_len - visible) as u16
        }
    }
}

// =============================================================================
// OverlayState impl
// =============================================================================

impl OverlayState for ConfirmOverlay {
    fn render<Wr: Write>(
        &self,
        w: &mut Wr,
        screen: Area,
        palette: &StyleMap,
    ) -> io::Result<()> {
        // ---- geometry ---------------------------------------------------
        let want_h: u16 = (self.body.len() as u16).saturating_add(5).min(15);
        let area = frame::centered_rect(screen, 50, want_h);
        if area.width < 8 || area.height < 5 {
            // Too small to draw anything sensible — bail. This avoids
            // a corrupted modal on tiny terminals.
            return Ok(());
        }

        // ---- background clear ------------------------------------------
        // Paint spaces row-by-row so we don't depend on `Clear` (which
        // resets attributes globally). Use the default style so the
        // modal sits on a clean canvas.
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
        frame::draw_frame_title(w, area.clone(), palette, &self.title, false)?;

        // ---- body -------------------------------------------------------
        let visible = Self::visible_body_rows(area.clone());
        let max_scroll = Self::max_scroll(self.body.len(), visible);
        let scroll = self.scroll.min(max_scroll) as usize;
        let body_top = area.top + 1;
        let inner_left = area.left + 2;
        let inner_width = area.width.saturating_sub(4) as usize;
        let body_style = &palette.default;

        let total = self.body.len();
        let visible_usize = visible as usize;
        let last_visible_idx = scroll + visible_usize;
        let overflow = total > last_visible_idx;

        for row in 0..visible {
            let body_idx = scroll + row as usize;
            if body_idx >= total {
                break;
            }
            w.queue(cursor::MoveTo(inner_left, body_top + row))?;
            let line = &self.body[body_idx];
            // Truncate the line to the available width; if this is the
            // last visible row and there is more body, append `…`.
            let is_last_visible = row + 1 == visible || body_idx + 1 == total;
            let display = if overflow && is_last_visible && body_idx + 1 < total {
                truncate_with_ellipsis(line, inner_width.saturating_sub(2))
            } else {
                truncate_to_width(line, inner_width)
            };
            body_style.queue_str(w, display).map_err(io_err)?;
        }

        // ---- button row -------------------------------------------------
        // Row sits at area.top + area.height - 2 (one above the bottom
        // border).
        let button_row_y = area.top + area.height - 2;
        let half = area.width / 2;

        let cancel_text = " [ Cancel ] ";
        let confirm_text = format!(" [ {} ] ", self.confirm_label);

        // Layout the two buttons left/right of the centre line.
        // Cancel sits in the left half, confirm in the right half.
        let cancel_x = area.left + 1;
        let cancel_w = (cancel_text.chars().count() as u16).min(half.saturating_sub(2));
        let confirm_w =
            (confirm_text.chars().count() as u16).min(area.width.saturating_sub(half + 2));
        let confirm_x = area.left + area.width - 1 - confirm_w;

        let cancel_focused = matches!(self.focus, ConfirmFocus::Cancel);
        let confirm_focused = matches!(self.focus, ConfirmFocus::Confirm);

        let cancel_style: &CompoundStyle = if cancel_focused {
            &palette.selected_line
        } else {
            &palette.default
        };
        let confirm_style: &CompoundStyle = if self.danger {
            // Danger: prefer file_error (red); when focused, layer with
            // selected_line for a visible "pressed" state.
            if confirm_focused {
                &palette.status_error
            } else {
                &palette.file_error
            }
        } else if confirm_focused {
            &palette.selected_line
        } else {
            &palette.default
        };

        w.queue(cursor::MoveTo(cancel_x, button_row_y))?;
        cancel_style
            .queue_str(w, truncate_to_width(cancel_text, cancel_w as usize))
            .map_err(io_err)?;

        w.queue(cursor::MoveTo(confirm_x, button_row_y))?;
        confirm_style
            .queue_str(w, truncate_to_width(&confirm_text, confirm_w as usize))
            .map_err(io_err)?;

        // Cache hit-rects.
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
        // Cancel-class keys.
        if key == key!(esc) || key == key!(ctrl - c) {
            return OverlayOutcome::Close;
        }
        if key == key!('n') || key == key!(shift - 'n') {
            return OverlayOutcome::Close;
        }

        // Direct-confirm.
        if key == key!('y') || key == key!(shift - 'y') {
            return OverlayOutcome::CloseAndRun(self.pending.clone());
        }

        // Focus toggle.
        if key == key!(tab) || key == key!(left) || key == key!(right) {
            self.focus = match self.focus {
                ConfirmFocus::Cancel => ConfirmFocus::Confirm,
                ConfirmFocus::Confirm => ConfirmFocus::Cancel,
            };
            return OverlayOutcome::Stay;
        }

        // Enter commits the focused button.
        if key == key!(enter) {
            return match self.focus {
                ConfirmFocus::Confirm => OverlayOutcome::CloseAndRun(self.pending.clone()),
                ConfirmFocus::Cancel => OverlayOutcome::Close,
            };
        }

        // Body scroll. We don't know the visible-row count without a
        // render area, so we clamp against the body length itself —
        // `render` re-clamps to the actual visible window.
        if key == key!(down) {
            let max = self.body.len().saturating_sub(1) as u16;
            if self.scroll < max {
                self.scroll += 1;
            }
            return OverlayOutcome::Stay;
        }
        if key == key!(up) {
            self.scroll = self.scroll.saturating_sub(1);
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
            return OverlayOutcome::CloseAndRun(self.pending.clone());
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

fn truncate_with_ellipsis(
    s: &str,
    max_w: usize,
) -> String {
    // Leave at least 2 cols for " …" suffix.
    if max_w < 2 {
        return truncate_to_width(s, max_w);
    }
    let body = truncate_to_width(s, max_w.saturating_sub(2));
    format!("{body} …")
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use {
        super::*,
        crokey::crossterm::event::KeyModifiers,
    };

    fn cmd() -> Command {
        Command::from_raw(":help".to_string(), true)
    }

    fn make(
        body: Vec<&str>,
        danger: bool,
    ) -> ConfirmOverlay {
        ConfirmOverlay::new(
            "Delete file?",
            body.into_iter().map(String::from).collect(),
            "Delete",
            danger,
            cmd(),
        )
    }

    fn mouse_at(
        x: u16,
        y: u16,
    ) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }
    }

    // ---- key dispatch ---------------------------------------------------

    #[test]
    fn tab_toggles_focus() {
        let mut o = make(vec!["/tmp/x"], false);
        assert_eq!(o.focus, ConfirmFocus::Cancel);
        let r = o.handle_key(key!(tab));
        assert!(matches!(r, OverlayOutcome::Stay));
        assert_eq!(o.focus, ConfirmFocus::Confirm);
        let _ = o.handle_key(key!(tab));
        assert_eq!(o.focus, ConfirmFocus::Cancel);
    }

    #[test]
    fn arrow_keys_toggle_focus() {
        let mut o = make(vec!["/tmp/x"], false);
        assert_eq!(o.focus, ConfirmFocus::Cancel);
        let _ = o.handle_key(key!(right));
        assert_eq!(o.focus, ConfirmFocus::Confirm);
        let _ = o.handle_key(key!(left));
        assert_eq!(o.focus, ConfirmFocus::Cancel);
    }

    #[test]
    fn enter_on_cancel_closes() {
        let mut o = make(vec!["/tmp/x"], false);
        assert_eq!(o.focus, ConfirmFocus::Cancel);
        let r = o.handle_key(key!(enter));
        assert!(matches!(r, OverlayOutcome::Close));
    }

    #[test]
    fn enter_on_confirm_runs_command() {
        let mut o = make(vec!["/tmp/x"], false);
        o.focus = ConfirmFocus::Confirm;
        let r = o.handle_key(key!(enter));
        assert!(matches!(r, OverlayOutcome::CloseAndRun(_)));
    }

    #[test]
    fn y_always_confirms() {
        let mut o = make(vec!["/tmp/x"], false);
        // even when focus is on Cancel
        assert_eq!(o.focus, ConfirmFocus::Cancel);
        let r = o.handle_key(key!('y'));
        assert!(matches!(r, OverlayOutcome::CloseAndRun(_)));
    }

    #[test]
    fn shift_y_also_confirms() {
        let mut o = make(vec!["/tmp/x"], false);
        let r = o.handle_key(key!(shift - 'y'));
        assert!(matches!(r, OverlayOutcome::CloseAndRun(_)));
    }

    #[test]
    fn n_always_cancels() {
        let mut o = make(vec!["/tmp/x"], false);
        o.focus = ConfirmFocus::Confirm;
        let r = o.handle_key(key!('n'));
        assert!(matches!(r, OverlayOutcome::Close));
    }

    #[test]
    fn esc_cancels() {
        let mut o = make(vec!["/tmp/x"], false);
        o.focus = ConfirmFocus::Confirm;
        let r = o.handle_key(key!(esc));
        assert!(matches!(r, OverlayOutcome::Close));
    }

    #[test]
    fn ctrl_c_cancels() {
        let mut o = make(vec!["/tmp/x"], false);
        o.focus = ConfirmFocus::Confirm;
        let r = o.handle_key(key!(ctrl - c));
        assert!(matches!(r, OverlayOutcome::Close));
    }

    #[test]
    fn down_scrolls_when_body_overflows() {
        let body: Vec<&str> = (0..20).map(|_| "row").collect();
        let mut o = make(body, false);
        assert_eq!(o.scroll, 0);
        let _ = o.handle_key(key!(down));
        assert_eq!(o.scroll, 1);
    }

    #[test]
    fn render_after_scroll_renders_later_body_lines() {
        // Drive the real render path with a body that overflows the
        // visible window, advance scroll past the first row, and assert
        // the captured bytes contain a body line that only appears
        // *after* scrolling. `BufWriter::buffer()` lets us inspect
        // the unflushed bytes — this is the actual render output, not
        // a sidecar buffer.
        let body: Vec<String> = (0..30).map(|i| format!("UNIQUE_LINE_{i}")).collect();
        let mut o = ConfirmOverlay::new("title", body, "OK", false, cmd());
        // Scroll past the first 10 rows.
        for _ in 0..15 {
            let _ = o.handle_key(key!(down));
        }
        let palette = StyleMap::no_term();
        let mut wbuf =
            std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 80, 24);
        o.render(&mut wbuf, screen, &palette).unwrap();
        let bytes = wbuf.buffer().to_vec();
        let s = String::from_utf8_lossy(&bytes);
        // Compute the post-scroll visible window explicitly so the
        // assertion fails loudly if the geometry changes:
        //   want_h = body.len()+5 capped at 15 → 15
        //   visible_body_rows = 15 - 4 = 11
        //   scroll = 15 (15 ↓ presses, clamped to max_scroll = 30-11 = 19)
        //   visible range = [15, 26).
        //
        // The last visible row can be ellipsis-truncated when the body
        // overflows ("UNIQUE_LINE_25 …"). To keep the assertion
        // robust we exclude index 25 — the line immediately before
        // (index 24) is rendered in full, which is enough to prove
        // scrolling actually happened. The original `(12..28)` window
        // both included indices that should never appear (≤14, ≥26)
        // and silently tolerated breakages within them.
        let visible_first = 15;
        let visible_last_full = 24; // exclusive bound 25 (ellipsis row).
        let any_late =
            (visible_first..visible_last_full + 1).any(|i| s.contains(&format!("UNIQUE_LINE_{i}")));
        assert!(
            any_late,
            "expected a post-scroll body line in render bytes; got: {s:?}",
        );
    }

    #[test]
    fn up_clamps_at_zero() {
        let mut o = make(vec!["a", "b"], false);
        assert_eq!(o.scroll, 0);
        let _ = o.handle_key(key!(up));
        assert_eq!(o.scroll, 0);
    }

    #[test]
    fn down_clamps_at_body_end() {
        // body has 3 rows; body.len() - 1 = 2; scroll cannot exceed 2.
        let mut o = make(vec!["a", "b", "c"], false);
        for _ in 0..10 {
            let _ = o.handle_key(key!(down));
        }
        assert!(o.scroll <= 2, "scroll {} exceeded clamp", o.scroll);
    }

    #[test]
    fn unrecognized_key_is_consumed_silently() {
        let mut o = make(vec!["/tmp/x"], false);
        let r = o.handle_key(key!('q'));
        assert!(matches!(r, OverlayOutcome::Stay));
    }

    // ---- mouse routing --------------------------------------------------

    #[test]
    fn mouse_on_confirm_runs_command() {
        // The trait `render` writes to a `BufWriter<Sink>` here so the
        // bytes are discarded entirely — tests don't observe them, and
        // cargo's stderr stays clean. We only need the *cached
        // hit-rects* the render path populates as a side effect.
        let mut o = make(vec!["/tmp/x"], false);
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 80, 24);
        o.render(&mut wbuf, screen, &palette).unwrap();
        let hits = o
            .button_hits
            .get_cloned()
            .expect("hit-rects must be cached after render");
        let cx = hits.confirm.left + hits.confirm.width / 2;
        let cy = hits.confirm.top;
        let r = o.handle_mouse(mouse_at(cx, cy));
        assert!(matches!(r, OverlayOutcome::CloseAndRun(_)));
    }

    #[test]
    fn mouse_on_cancel_closes() {
        let mut o = make(vec!["/tmp/x"], false);
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 80, 24);
        o.render(&mut wbuf, screen, &palette).unwrap();
        let hits = o.button_hits.get_cloned().unwrap();
        let cx = hits.cancel.left + hits.cancel.width / 2;
        let cy = hits.cancel.top;
        let r = o.handle_mouse(mouse_at(cx, cy));
        assert!(matches!(r, OverlayOutcome::Close));
    }

    #[test]
    fn mouse_off_buttons_stays() {
        let mut o = make(vec!["/tmp/x"], false);
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 80, 24);
        o.render(&mut wbuf, screen, &palette).unwrap();
        // top-left corner of screen is well outside both button rects.
        let r = o.handle_mouse(mouse_at(0, 0));
        assert!(matches!(r, OverlayOutcome::Stay));
    }

    #[test]
    fn mouse_non_left_click_stays() {
        let mut o = make(vec!["/tmp/x"], false);
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 80, 24);
        o.render(&mut wbuf, screen, &palette).unwrap();
        let hits = o.button_hits.get_cloned().unwrap();
        let cx = hits.confirm.left + hits.confirm.width / 2;
        let cy = hits.confirm.top;
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
        // Without a render call the hit-rect cache is empty; clicks
        // must be ignored gracefully (Stay), not panic or fire a
        // command. Pins the `Cell::take`-based cache invariant.
        let mut o = make(vec!["/tmp/x"], false);
        let r = o.handle_mouse(mouse_at(40, 12));
        assert!(matches!(r, OverlayOutcome::Stay));
    }

    // ---- render shape ---------------------------------------------------

    #[test]
    fn render_writes_corners_and_title_and_buttons() {
        // Drive the real `OverlayState::render` and capture its output
        // from the `BufWriter<Sink>` buffer *before* flushing. This is
        // the actual render path, not a sidecar buffer mimicking it.
        // The capture works because `BufWriter::buffer()` returns a
        // borrow of unflushed bytes — we simply skip the `flush` call.
        // Backing the buffer with `sink()` keeps cargo test's stderr
        // free of noise (unflushed bytes never reach a real fd).
        let palette = StyleMap::no_term();
        let o = make(vec!["/tmp/a", "/tmp/b"], false);
        // 64 KiB is comfortably larger than the 80x24 modal output, so
        // the buffer never auto-flushes during the render.
        let mut wbuf =
            std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 80, 24);
        o.render(&mut wbuf, screen, &palette).unwrap();
        let bytes = wbuf.buffer().to_vec();
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains('╭'), "missing top-left corner in render bytes");
        assert!(s.contains('╰'), "missing bottom-left corner in render bytes");
        assert!(s.contains("Delete file?"), "missing title in render bytes: {s:?}");
        assert!(s.contains("Cancel"), "missing Cancel button label in render bytes");
        assert!(s.contains("Delete"), "missing Delete confirm label in render bytes");
    }

    // The previous `danger_render_differs_from_safe` test only asserted
    // on the `danger` flag (because `StyleMap::no_term` flattens the
    // palette to empty styles, the two renders are byte-identical). It
    // was removed because it does not actually exercise the styling
    // code path. The real render path is exercised by
    // `render_caches_button_hits` and the structural-pin behaviour
    // tests above.

    #[test]
    fn render_caches_button_hits() {
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let o = make(vec!["/tmp/a"], false);
        let screen = Area::new(0, 0, 80, 24);
        assert!(o.button_hits.get_cloned().is_none());
        o.render(&mut wbuf, screen, &palette).unwrap();
        let hits = o
            .button_hits
            .get_cloned()
            .expect("hits should be populated after render");
        // Both buttons must have non-zero width.
        assert!(hits.cancel.width > 0);
        assert!(hits.confirm.width > 0);
        // They must not overlap horizontally.
        let cancel_right = hits.cancel.left + hits.cancel.width;
        assert!(
            cancel_right <= hits.confirm.left,
            "cancel and confirm rects overlap: {hits:?}"
        );
    }

    #[test]
    fn render_clamps_oversize_body() {
        // 30 lines of body, screen 24 rows — height clamps to 15.
        let body: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let o = ConfirmOverlay::new("t", body, "OK", false, cmd());
        let screen = Area::new(0, 0, 80, 24);
        // Should not panic, should populate hits.
        o.render(&mut wbuf, screen, &palette).unwrap();
        assert!(o.button_hits.get_cloned().is_some());
    }

}
